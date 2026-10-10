//! soundserver as the driver of an Intel HDA controller.
//!
//! The kernel brought the controller up, owns the buffer descriptor list and
//! the interrupt, and answers five register writes and two reads. Everything
//! that is a *decision* is here and in `toyos-hda` — which codecs answered,
//! which pin, which converter, the amplifiers, EAPD, the format — and every one
//! of those decisions is a pure function this file calls.
//!
//! What this file itself owns is the I/O: one verb over the immediate-command
//! registers, and the stream's run bit.

use toyos::HdaDev;
use toyos_abi::hda::HdaInfo;
use toyos_abi::syscall::{RegWidth, SyscallError};
use toyos_hda::caps::{AmpCaps, PcmCaps};
use toyos_hda::graph::{Codec, FunctionGroup, FunctionKind};
use toyos_hda::path::{OutputPath, PathError, PinSetup};
use toyos_hda::verb::{self as verb, Address, Node, Response, Verb};
use toyos_hda::{config, probe};

/// The controller's immediate-command registers, as byte offsets into the
/// register window. The kernel's allow-list carries the same three numbers and
/// refuses everything else.
const IMMEDIATE_COMMAND: u32 = 0x60;
const IMMEDIATE_RESPONSE: u32 = 0x64;
const IMMEDIATE_STATUS: u32 = 0x68;

const IMMEDIATE_BUSY: u32 = 1 << 0;
const IMMEDIATE_RESULT_VALID: u32 = 1 << 1;

/// Stream descriptor fields, relative to the descriptor the kernel reported.
const SD_CTL: u32 = 0x00;
const SD_CTL_TAG: u32 = 0x02;
const SD_FMT: u32 = 0x12;

const SD_CTL_RUN: u32 = 1 << 1;
const SD_CTL_IOCE: u32 = 1 << 2;
const SD_CTL_FEIE: u32 = 1 << 3;
const SD_CTL_DEIE: u32 = 1 << 4;

/// How many times a verb's completion is polled before the controller is called
/// silent.
///
/// Policy: the specification completes an immediate command in one codec frame,
/// about 21 µs at 48 kHz, and each poll here is a syscall. A driver that spun
/// forever on a controller with no verb interface would take the machine's
/// audio down with a hang instead of a refusal.
const VERB_POLLS: u32 = 4096;

pub struct Hda {
    dev: HdaDev,
    info: HdaInfo,
    /// Set once the engine has been told to run, so a stop and a resume are one
    /// register write each and not one per period.
    running: bool,
}

/// Why this machine's HDA controller cannot carry audio. Each is a line soundserver
/// prints before falling back to the null sink — "no sound" without which of
/// these it was is a report nobody can act on.
pub enum Refusal {
    /// The kernel's answers stopped making sense, which is a bug here or there
    /// and never a property of the machine.
    Kernel(SyscallError),
    /// No codec `STATESTS` named answered a verb.
    NoCodec,
    /// Every codec answered and none offers an output a human can hear.
    NoOutput(toyos_hda::PathError),
    /// Neither the converter behind the chosen pin nor its function group
    /// offers any rate this pipeline runs at.
    Rate,
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Kernel(e) => write!(f, "the kernel refused a call this driver has to make ({e})"),
            Self::NoCodec => write!(f, "no codec STATESTS named answered a verb"),
            Self::NoOutput(PathError::NoOutputPin { codecs }) => {
                write!(f, "no output a human can hear, on codec")?;
                for (i, address) in codecs.iter().enumerate() {
                    write!(f, "{}{address}", if i == 0 { " " } else { ", " })?;
                }
                Ok(())
            }
            Self::NoOutput(PathError::Cycle { pin, at }) => {
                write!(f, "pin {:#04x}'s connection list leads back to {:#04x}", pin.0, at.0)
            }
            Self::NoOutput(PathError::OutsideGroup { pin, named }) => write!(
                f,
                "pin {:#04x} names node {:#04x}, which its function group never declared",
                pin.0, named.0
            ),
            Self::NoOutput(PathError::NoConverter { pin }) => {
                write!(f, "no converter behind pin {:#04x}", pin.0)
            }
            Self::Rate => write!(
                f,
                "the converter offers none of {:?} Hz at {} bits",
                config::RATES,
                config::WIDTH
            ),
        }
    }
}

impl Hda {
    /// Walk the controller's codecs, choose an output and configure it.
    ///
    /// The claim is the argument: `/system/bin/supervisor` minted it and endowed it, so
    /// "does this machine have an HDA?" was already answered before soundserver's
    /// first instruction.
    pub fn claim(dev: HdaDev) -> Result<(Self, OutputPath, u8, u32), Refusal> {
        let info = dev.info().map_err(Refusal::Kernel)?;
        let mut hda = Hda { dev, info, running: false };

        let found = probe::enumerate(&mut hda, info.statests);
        let mut codecs: Vec<Codec> = Vec::new();
        for entry in found {
            match entry {
                Ok(codec) => {
                    say!(
                        "soundserver: hda codec{} vendor={:04x} device={:04x}, {} function group(s)",
                        codec.address,
                        codec.vendor,
                        codec.device,
                        codec.groups.len()
                    );
                    codecs.push(codec);
                }
                Err((address, fault)) => {
                    say!("soundserver: hda codec{address} answered nothing usable ({fault:?})")
                }
            }
        }
        if codecs.is_empty() {
            return Err(Refusal::NoCodec);
        }

        // YOGA HACK: the whole graph, said, before anything is chosen.
        let mut group_pcm = None;
        for codec in &codecs {
            for group in &codec.groups {
                let a = codec.address;
                let pcm = hda.get(a, group.node, verb::GET_PARAMETER, verb::PARAM_PCM);
                let formats = hda.get(a, group.node, verb::GET_PARAMETER, verb::PARAM_STREAM_FORMATS);
                let subsystem = hda.get(a, group.node, GET_SUBSYSTEM_ID, 0);
                say!(
                    "soundserver: YOGA codec{a} group {:#04x} ({}): pcm {} formats {} subsystem {}",
                    group.node.0,
                    group.kind.name(),
                    word(pcm),
                    word(formats),
                    word(subsystem),
                );
                if let Some(r) = pcm {
                    say!("soundserver: YOGA   group pcm: {}", pcm_text(PcmCaps::decode(r)));
                }
                if group.kind == FunctionKind::Audio && group_pcm.is_none() {
                    group_pcm = pcm.map(PcmCaps::decode);
                }
                for w in &group.widgets {
                    say!(
                        "soundserver: YOGA   node {:#04x} {} ch={} conns={:02x?} amp_in={:?} amp_out={:?} power={} override(fmt={} amp={}){}{}",
                        w.node.0,
                        w.caps.kind.name(),
                        w.caps.channels,
                        w.connections.iter().map(|n| n.0).collect::<Vec<_>>(),
                        w.amp_in,
                        w.amp_out,
                        w.caps.power_control,
                        w.caps.format_override,
                        w.caps.amp_override,
                        match w.pcm {
                            Some(pcm) => format!(" pcm {}", pcm_text(pcm)),
                            None => String::new(),
                        },
                        match &w.pin {
                            Some(pin) => format!(" pin {:?} {:?}", pin.config, pin.caps),
                            None => String::new(),
                        },
                    );
                }
            }
        }

        let path = toyos_hda::find_output_path(&codecs).map_err(Refusal::NoOutput)?;
        say!("soundserver: YOGA chosen path {path:?}");
        let group = codecs
            .iter()
            .find(|c| c.address == path.codec)
            .and_then(|c| c.groups.iter().find(|g| g.node == path.group))
            .expect("the path names a group this walk produced")
            .clone();
        let chains: Vec<(Node, Vec<(Node, u8)>)> = core::iter::once(&path.output)
            .chain(path.headphone.iter())
            .map(|pin| (pin.node, chain(&group, pin)))
            .collect();
        for (pin, hops) in &chains {
            say!("soundserver: YOGA chain from pin {:#04x}: {:02x?} (node, input index)", pin.0, hops);
        }

        let (format, channels, rate) =
            config::format(&codecs, &path, group_pcm).ok_or(Refusal::Rate)?;
        say!(
            "soundserver: hda codec{} group {:#04x} converter {:#04x} -> pin {:#04x} ({}), \
             headphone {}, format {:#06x} ({} Hz {} ch {}-bit)",
            path.codec,
            path.group.0,
            path.converter.0,
            path.output.node.0,
            path.device.name(),
            match &path.headphone {
                Some(hp) => alloc_node(hp.node),
                None => String::from("none"),
            },
            format,
            rate,
            channels,
            config::WIDTH,
        );

        let mut verbs = config::verbs(&codecs, &path, format, info.stream_tag)
            .expect("the path names a codec this walk produced");
        // YOGA HACK: every widget between a pin and the converter that the
        // shared sequence does not reach — a single-connection mixer or
        // selector — powered, and its amplifiers unmuted at 0 dB, the input
        // one on the index the chain takes.
        let mut extra = Vec::new();
        for (_, hops) in &chains {
            for &(node, index) in hops {
                let w = group.widget(node).expect("the chain walked this widget");
                if w.pin.is_some() || w.is_converter() {
                    continue;
                }
                if w.caps.power_control {
                    extra.push(Verb::short(path.codec, node, verb::SET_POWER_STATE, 0));
                }
                if let Some(amp) = w.amp_out {
                    extra.push(amp_set(path.codec, node, AMP_OUTPUT, 0, amp));
                }
                if let Some(amp) = w.amp_in {
                    extra.push(amp_set(path.codec, node, AMP_INPUT, index, amp));
                }
            }
        }
        // Before the converter's format and tag, which close the sequence.
        let at = verbs.len() - 2;
        verbs.splice(at..at, extra);

        let sent = verbs.len();
        for v in verbs {
            let response = hda.send(v);
            say!("soundserver: YOGA verb {:#010x} -> {}", v.raw(), word(response));
        }

        // YOGA HACK: what the codec says it now holds, on every node of every chain.
        for (pin, hops) in &chains {
            for &(node, index) in hops {
                let a = path.codec;
                let w = group.widget(node).expect("the chain walked this widget");
                let power = hda.get(a, node, verb::GET_POWER_STATE, 0);
                let out_l = hda.send(Verb::long(a, node, GET_AMP_GAIN_MUTE, 0x8000 | 0x2000));
                let out_r = hda.send(Verb::long(a, node, GET_AMP_GAIN_MUTE, 0x8000));
                let in_l = hda.send(Verb::long(a, node, GET_AMP_GAIN_MUTE, 0x2000 | index as u16));
                let in_r = hda.send(Verb::long(a, node, GET_AMP_GAIN_MUTE, index as u16));
                let mut line = format!(
                    "soundserver: YOGA readback pin {:#04x} node {:#04x} {}: power {} amp_out L {} R {} amp_in[{index}] L {} R {}",
                    pin.0,
                    node.0,
                    w.caps.kind.name(),
                    word(power),
                    word(out_l),
                    word(out_r),
                    word(in_l),
                    word(in_r),
                );
                if w.connections.len() > 1 {
                    let select = hda.get(a, node, verb::GET_CONNECTION_SELECT, 0);
                    line += &format!(" select {}", word(select));
                }
                if w.pin.is_some() {
                    let control = hda.get(a, node, verb::GET_PIN_CONTROL, 0);
                    let eapd = hda.get(a, node, verb::GET_EAPD, 0);
                    let sense = hda.get(a, node, verb::GET_PIN_SENSE, 0);
                    line += &format!(" pinctl {} eapd {} sense {}", word(control), word(eapd), word(sense));
                }
                if w.is_converter() {
                    let fmt = hda.send(Verb::long(a, node, verb::GET_CONVERTER_FORMAT as u8, 0));
                    let stream = hda.get(a, node, verb::GET_CONVERTER_STREAM, 0);
                    line += &format!(" format {} stream {}", word(fmt), word(stream));
                }
                say!("{line}");
            }
        }

        // The tag before the format, and both before the engine is ever told to
        // run: a descriptor that starts with neither plays whatever the last
        // owner of the stream left in it.
        hda.write(SD_CTL_TAG, RegWidth::U8, (info.stream_tag as u32) << 4)
            .map_err(Refusal::Kernel)?;
        hda.write(SD_FMT, RegWidth::U16, format as u32).map_err(Refusal::Kernel)?;
        say!("soundserver: hda path configured in {sent} verbs, stream tag {}", info.stream_tag);
        Ok((hda, path, channels, rate))
    }

    pub fn info(&self) -> HdaInfo {
        self.info
    }

    pub fn dev(&self) -> &HdaDev {
        &self.dev
    }

    /// Start the engine, which is one register write and only on the edge.
    ///
    /// There is no per-period submit: `SDnLVI` and the descriptor list are the
    /// kernel's and the engine cycles them unaided, so a period costs this
    /// driver a read of the completion record and no register access at all.
    pub fn start(&mut self) {
        if self.running {
            return;
        }
        if let Err(e) = self.write(SD_CTL, RegWidth::U8, SD_CTL_RUN | SD_CTL_IOCE | SD_CTL_FEIE | SD_CTL_DEIE) {
            panic!("soundserver: hda could not start its stream: {e:?}");
        }
        self.running = true;
    }

    pub fn stop(&mut self) {
        if !self.running {
            return;
        }
        if let Err(e) = self.write(SD_CTL, RegWidth::U8, SD_CTL_IOCE | SD_CTL_FEIE | SD_CTL_DEIE) {
            panic!("soundserver: hda could not stop its stream: {e:?}");
        }
        self.running = false;
    }

    fn write(&self, field: u32, width: RegWidth, value: u32) -> Result<(), SyscallError> {
        self.dev.reg_write(self.info.stream_offset + field, width, value)
    }

    /// One verb over the immediate-command registers: two writes, a bounded
    /// poll, and a read.
    ///
    /// CORB/RIRB would batch several verbs behind one ring-pointer write, and
    /// there is nothing here to batch for: verbs are sent at claim time and on a
    /// jack poll, never in the audio path. What the ring would buy is a
    /// syscall this driver spends about a hundred times per boot.
    fn send(&mut self, verb: Verb) -> Option<Response> {
        let dev = &self.dev;
        if !self.settles(|status| status & IMMEDIATE_BUSY == 0) {
            return None;
        }
        dev.reg_write(IMMEDIATE_STATUS, RegWidth::U16, IMMEDIATE_RESULT_VALID).ok()?;
        dev.reg_write(IMMEDIATE_COMMAND, RegWidth::U32, verb.raw()).ok()?;
        dev.reg_write(IMMEDIATE_STATUS, RegWidth::U16, IMMEDIATE_BUSY).ok()?;
        if !self.settles(|status| {
            status & IMMEDIATE_BUSY == 0 && status & IMMEDIATE_RESULT_VALID != 0
        }) {
            return None;
        }
        let response = dev.reg_read(IMMEDIATE_RESPONSE, RegWidth::U32).ok()?;
        dev.reg_write(IMMEDIATE_STATUS, RegWidth::U16, IMMEDIATE_RESULT_VALID).ok()?;
        Response::new(response)
    }

    fn settles(&self, ready: impl Fn(u32) -> bool) -> bool {
        for _ in 0..VERB_POLLS {
            match self.dev.reg_read(IMMEDIATE_STATUS, RegWidth::U16) {
                Ok(status) if ready(status) => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    }
}

impl Hda {
    fn get(&mut self, codec: Address, node: Node, verb: u16, payload: u8) -> Option<Response> {
        self.send(Verb::short(codec, node, verb, payload))
    }
}

/// YOGA HACK: the verbs and payload bits only the survey above uses.
const GET_SUBSYSTEM_ID: u16 = 0xF20;
const GET_AMP_GAIN_MUTE: u8 = 0xB;
const AMP_OUTPUT: u16 = 1 << 15;
const AMP_INPUT: u16 = 1 << 14;

/// Unmuted, both channels, at the amplifier's 0 dB index where it has one.
fn amp_set(codec: Address, node: Node, which: u16, index: u8, amp: AmpCaps) -> Verb {
    let gain = amp.gain.map_or(0, |range| range.zero_db as u16);
    let payload = which | (1 << 13) | (1 << 12) | ((index as u16 & 0xF) << 8) | gain;
    Verb::long(codec, node, verb::SET_AMP_GAIN_MUTE as u8, payload)
}

/// From `pin` inward to the converter: each node and the input index taken
/// out of it, the hop's select where it has one and its only input otherwise.
fn chain(group: &FunctionGroup, pin: &PinSetup) -> Vec<(Node, u8)> {
    let mut out = Vec::new();
    let mut node = pin.node;
    for _ in 0..16 {
        let Some(w) = group.widget(node) else { break };
        let index = pin.route.iter().find(|h| h.node == node).map_or(0, |h| h.select);
        out.push((node, index));
        if w.is_converter() {
            break;
        }
        let Some(&next) = w.connections.get(index as usize) else { break };
        node = next;
    }
    out
}

fn word(response: Option<Response>) -> String {
    response.map_or(String::from("none"), |r| format!("{:#010x}", r.raw()))
}

fn pcm_text(pcm: PcmCaps) -> String {
    format!(
        "{:#010x} rates {:?} widths {:?}",
        pcm.raw(),
        pcm.rates().collect::<Vec<_>>(),
        pcm.widths().collect::<Vec<_>>()
    )
}

impl probe::Verbs for Hda {
    fn get(&mut self, codec: Address, node: Node, verb: u16, payload: u8) -> Option<Response> {
        self.send(Verb::short(codec, node, verb, payload))
    }
}

fn alloc_node(node: Node) -> String {
    format!("{:#04x}", node.0)
}
