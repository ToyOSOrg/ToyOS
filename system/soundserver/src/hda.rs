//! soundserver as the driver of an Intel HDA controller.
//!
//! The kernel lists every controller it can bind, owns the buffer descriptor
//! list and the interrupt, and answers five register writes, two reads and a
//! bind. Everything that is a *decision* is here and in `toyos-hda` — which
//! controller, which codecs answered, which pin, which converter, the rate, the
//! amplifiers, EAPD, the format and the master — and every one of those
//! decisions is a pure function this file calls.
//!
//! What this file itself owns is the I/O: one verb over the immediate-command
//! registers, the bind, and the stream's run bit. After the claim, the control
//! thread owns the codec's verbs ([`Codec`]) and the mix thread the run bit
//! ([`Hda`]).

use std::sync::Arc;

use toyos::HdaDev;
use toyos_abi::hda::{HdaController, HdaInfo, Reg};
use toyos_abi::syscall::{RegWidth, SyscallError};
use toyos_hda::caps::PcmCaps;
use toyos_hda::config::{self, Master, NoMaster, Setting};
use toyos_hda::graph::Codec as Graph;
use toyos_hda::path::{PathError, Unchosen};
use toyos_hda::verb::{Address, Node, Response, Verb};
use toyos_hda::probe;

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

/// The bound controller's index once the bind has run: the claim lists it alone.
const BOUND: u8 = 0;

/// One controller's verb interface: the claim, and the controller's index in
/// the claim's current list.
struct Link {
    dev: Arc<HdaDev>,
    controller: u8,
}

impl Link {
    fn reg(&self, offset: u32) -> Reg {
        Reg { controller: self.controller, offset }
    }

    /// One verb over the immediate-command registers: two writes, a bounded
    /// poll, and a read.
    ///
    /// CORB/RIRB would batch several verbs behind one ring-pointer write, and
    /// there is nothing here to batch for: verbs are sent at claim time and on
    /// a press of a volume key, never in the audio path.
    fn send(&self, verb: Verb) -> Option<Response> {
        let dev = &self.dev;
        if !self.settles(|status| status & IMMEDIATE_BUSY == 0) {
            return None;
        }
        dev.reg_write(self.reg(IMMEDIATE_STATUS), RegWidth::U16, IMMEDIATE_RESULT_VALID).ok()?;
        dev.reg_write(self.reg(IMMEDIATE_COMMAND), RegWidth::U32, verb.raw()).ok()?;
        dev.reg_write(self.reg(IMMEDIATE_STATUS), RegWidth::U16, IMMEDIATE_BUSY).ok()?;
        if !self.settles(|status| {
            status & IMMEDIATE_BUSY == 0 && status & IMMEDIATE_RESULT_VALID != 0
        }) {
            return None;
        }
        let response = dev.reg_read(self.reg(IMMEDIATE_RESPONSE), RegWidth::U32).ok()?;
        dev.reg_write(self.reg(IMMEDIATE_STATUS), RegWidth::U16, IMMEDIATE_RESULT_VALID).ok()?;
        Response::new(response)
    }

    fn settles(&self, ready: impl Fn(u32) -> bool) -> bool {
        for _ in 0..VERB_POLLS {
            match self.dev.reg_read(self.reg(IMMEDIATE_STATUS), RegWidth::U16) {
                Ok(status) if ready(status) => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    }
}

impl probe::Verbs for Link {
    fn get(&mut self, codec: Address, node: Node, verb: u16, payload: u8) -> Option<Response> {
        self.send(Verb::short(codec, node, verb, payload))
    }
}

/// The stream: the mix thread's, which starts and stops it.
pub struct Hda {
    dev: Arc<HdaDev>,
    info: HdaInfo,
    stream_offset: u32,
    /// Set once the engine has been told to run, so a stop and a resume are one
    /// register write each and not one per period.
    running: bool,
}

/// The codec's master: the control thread's, which sets and reads it back.
pub struct Codec {
    link: Link,
    master: Master,
    setting: Setting,
}

/// What a claim of the HDA controllers came to.
pub struct Claimed {
    pub hda: Hda,
    pub rate: u32,
    pub channels: u8,
    /// `None` where the path has no master, which soundserver names and serves
    /// as unavailable.
    pub codec: Option<Codec>,
}

/// Why this machine's HDA controllers cannot carry audio. Each is a line
/// soundserver prints before falling back to the null sink — "no sound" without
/// which of these it was is a report nobody can act on.
pub enum Refusal {
    /// The kernel's answers stopped making sense, which is a bug here or there
    /// and never a property of the machine.
    Kernel(SyscallError),
    /// None of the listed controllers reaches an output, or several do.
    Choice(Vec<HdaController>, Unchosen),
    /// The kernel refused the bind; its `hda:` line says why.
    Bind(HdaController, SyscallError),
    /// The converter behind the chosen pin offers none of the device rates, at
    /// 16 bits; what it does offer, where it has a PCM word at all.
    Rate(Option<PcmCaps>),
}

/// `bb:dd.f`, as the kernel's `hda:` lines name a controller.
struct Pci<'a>(&'a HdaController);

impl core::fmt::Display for Pci<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:02x}:{:02x}.{}", self.0.bus, self.0.dev, self.0.func)
    }
}

fn path_error(f: &mut core::fmt::Formatter<'_>, why: &PathError) -> core::fmt::Result {
    match why {
        PathError::NoOutputPin { codecs } if codecs.is_empty() => {
            write!(f, "no codec answered")
        }
        PathError::NoOutputPin { codecs } => {
            write!(f, "no output a human can hear, on codec")?;
            for (i, address) in codecs.iter().enumerate() {
                write!(f, "{}{address}", if i == 0 { " " } else { ", " })?;
            }
            Ok(())
        }
        PathError::Cycle { pin, at } => {
            write!(f, "pin {:#04x}'s connection list leads back to {:#04x}", pin.0, at.0)
        }
        PathError::OutsideGroup { pin, named } => write!(
            f,
            "pin {:#04x} names node {:#04x}, which its function group never declared",
            pin.0, named.0
        ),
        PathError::NoConverter { pin } => write!(f, "no converter behind pin {:#04x}", pin.0),
    }
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Kernel(e) => write!(f, "the kernel refused a call this driver has to make ({e})"),
            Self::Choice(controllers, Unchosen(found)) => {
                let reaching = found.iter().filter(|walk| walk.is_ok()).count();
                if reaching == 0 {
                    write!(f, "no controller reaches an output")?;
                } else {
                    write!(f, "{reaching} controllers reach an output, and choosing between them is a policy nothing keeps")?;
                }
                for (controller, walk) in controllers.iter().zip(found) {
                    write!(f, "; {}: ", Pci(controller))?;
                    match walk {
                        Ok(path) => write!(f, "pin {:#04x} ({})", path.output.node.0, path.device.name())?,
                        Err(why) => path_error(f, why)?,
                    }
                }
                Ok(())
            }
            Self::Bind(controller, e) => {
                write!(f, "the kernel refused to bind {} ({e}); its `hda:` line says why", Pci(controller))
            }
            Self::Rate(None) => write!(f, "the converter has no PCM word"),
            Self::Rate(Some(pcm)) => {
                write!(f, "the converter offers none of {:?} Hz at {} bits; it offers", toyos_mixer::DEVICE_RATES, config::WIDTH)?;
                for rate in pcm.rates() {
                    write!(f, " {rate}")?;
                }
                write!(f, " Hz at {:?} bits", pcm.widths().collect::<Vec<_>>())
            }
        }
    }
}

impl core::fmt::Display for NoMasterLine {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            NoMaster::NoGain => write!(f, "no amplifier on the path has a gain range"),
            NoMaster::NoMute => write!(f, "neither an output pin nor the converter has a mute bit"),
        }
    }
}

struct NoMasterLine(NoMaster);

/// Every listed controller's codecs, walked over its own verb interface, in
/// list order.
fn walk(dev: &Arc<HdaDev>, listed: &[HdaController]) -> Vec<Vec<Graph>> {
    let mut walks = Vec::new();
    for (index, controller) in listed.iter().enumerate() {
        let mut link = Link { dev: dev.clone(), controller: index as u8 };
        let mut codecs = Vec::new();
        for entry in probe::enumerate(&mut link, controller.statests) {
            match entry {
                Ok(codec) => {
                    say!(
                        "soundserver: hda {} codec{} vendor={:04x} device={:04x}, {} function group(s)",
                        Pci(controller),
                        codec.address,
                        codec.vendor,
                        codec.device,
                        codec.groups.len()
                    );
                    codecs.push(codec);
                }
                Err((address, fault)) => say!(
                    "soundserver: hda {} codec{address} answered nothing usable ({fault:?})",
                    Pci(controller)
                ),
            }
        }
        walks.push(codecs);
    }
    walks
}

impl Hda {
    /// Walk every listed controller's codecs, choose the one controller whose
    /// graph reaches an output, bind it, and configure its path at the first
    /// device rate its converter offers.
    ///
    /// The claim is the argument: `/system/bin/supervisor` minted it and endowed it, so
    /// "does this machine have an HDA?" was already answered before soundserver's
    /// first instruction. Everything here runs before soundserver starts a thread.
    pub fn claim(dev: HdaDev) -> Result<Claimed, Refusal> {
        let info = dev.info().map_err(Refusal::Kernel)?;
        let listed = info.listed().to_vec();
        let dev = Arc::new(dev);

        let mut walks = walk(&dev, &listed);
        let (chosen, path) = toyos_hda::choose(&walks).map_err(|u| Refusal::Choice(listed.clone(), u))?;
        let controller = listed[chosen];
        dev.bind(chosen as u8).map_err(|e| Refusal::Bind(controller, e))?;
        for (index, other) in listed.iter().enumerate().filter(|&(i, _)| i != chosen) {
            say!("soundserver: hda bound {}, not {} (controller {index})", Pci(&controller), Pci(other));
        }
        let codecs = walks.swap_remove(chosen);

        let rate = config::rate(&codecs, &path, &toyos_mixer::DEVICE_RATES).map_err(Refusal::Rate)?;
        let (format, channels) =
            config::format(&codecs, &path, rate).expect("a rate the converter offers has a format");
        say!(
            "soundserver: hda {} codec{} group {:#04x} converter {:#04x} -> pin {:#04x} ({}), \
             headphone {}, format {:#06x} ({} Hz {} ch {}-bit)",
            Pci(&controller),
            path.codec,
            path.group.0,
            path.converter.0,
            path.output.node.0,
            path.device.name(),
            match &path.headphone {
                Some(hp) => format!("{:#04x}", hp.node.0),
                None => String::from("none"),
            },
            format,
            rate,
            channels,
            config::WIDTH,
        );

        let link = Link { dev: dev.clone(), controller: BOUND };
        let verbs = config::verbs(&codecs, &path, format, info.stream_tag)
            .expect("the path names a codec this walk produced");
        let sent = verbs.len();
        for verb in verbs {
            link.send(verb);
        }

        let hda = Hda { dev: dev.clone(), info, stream_offset: controller.stream_offset, running: false };
        // The tag before the format, and both before the engine is ever told to
        // run: a descriptor that starts with neither plays whatever the last
        // owner of the stream left in it.
        hda.write(SD_CTL_TAG, RegWidth::U8, (info.stream_tag as u32) << 4).map_err(Refusal::Kernel)?;
        hda.write(SD_FMT, RegWidth::U16, format as u32).map_err(Refusal::Kernel)?;
        say!("soundserver: hda path configured in {sent} verbs, stream tag {}", info.stream_tag);

        let codec = match config::master(&path) {
            Ok(master) => Some(Codec { link, master, setting: Setting::TOP }),
            Err(why) => {
                say!("soundserver: hda path has no master ({}); volume answers unavailable", NoMasterLine(why));
                None
            }
        };
        Ok(Claimed { hda, rate, channels, codec })
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
        let reg = Reg { controller: BOUND, offset: self.stream_offset + field };
        self.dev.reg_write(reg, width, value)
    }
}

/// What the master answers a request with: the setting read back from the
/// codec after it was set, never the intent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Answer {
    pub setting: Setting,
    pub bottom: u8,
}

impl Codec {
    /// Move the level by `delta` positions; a step while muted unmutes.
    pub fn step(&mut self, delta: i32) -> Option<Answer> {
        self.apply(self.master.step(self.setting, delta))
    }

    /// Mute, or unmute at the level it had.
    pub fn toggle_mute(&mut self) -> Option<Answer> {
        self.apply(Setting { muted: !self.setting.muted, ..self.setting })
    }

    /// Set `setting` and read it back. `None`, named, where a verb went
    /// unanswered or the answers name no setting.
    fn apply(&mut self, setting: Setting) -> Option<Answer> {
        for verb in self.master.set(setting) {
            self.link.send(verb)?;
        }
        let mut answers = Vec::new();
        for verb in self.master.read() {
            let Some(answer) = self.link.send(verb) else {
                say!("soundserver: hda master readback went unanswered");
                return None;
            };
            answers.push(answer);
        }
        let Some(read) = self.master.decode(&answers) else {
            say!("soundserver: hda master read back as no setting ({answers:?})");
            return None;
        };
        self.setting = read;
        Some(Answer { setting: read, bottom: self.master.bottom() })
    }
}
