//! soundserver as the driver of an Intel HDA controller it holds as a PCI
//! claim.
//!
//! What the kernel keeps is the claim: config space, the message it armed,
//! and the address space the function translates through. **The controller —
//! its reset, CORB and RIRB, the stream and its descriptor list — is
//! `toyos-hda`'s `controller`, and every decision about the codec is
//! `toyos-hda`'s too**: which pin, which converter, the rate, the amplifiers,
//! EAPD, the format and the master. This file is what is left: the claim's
//! two mappings, when its periods came back, and what becomes of a refusal.
//!
//! After the claim the control thread owns the codec link
//! ([`MasterControl`]) and the mix thread the stream ([`Hda`]); the two touch
//! disjoint registers.
//!
//! **A refusal after bring-up ends soundserver** by its own name: no
//! conforming controller writes one.

use std::sync::Arc;
use std::time::Duration;

use toyos::shm::SharedMemory;
use toyos::volatile::Window;
use toyos::{DmaRegion, PciDev};
use toyos_abi::syscall::SyscallError;
use toyos_hda::caps::PcmCaps;
use toyos_hda::config::{self, Master, NoMaster, Setting};
use toyos_hda::controller::{self, Link, LinkFault, Stream, CODEC_DETECT_MICROS, GRANT_BYTES, PERIOD_BYTES};
use toyos_hda::graph::Codec;
use toyos_hda::path::{PathError, Unchosen};
use toyos_hda::probe;
use toyos_pci_claim::{Bar, Grant, KernelRefused};

use crate::backend::Completion;

/// A multimedia audio device, class `04:03:00` (PCI Code and ID Assignment:
/// HD Audio, Intel HDA 1.0 compatible), whoever made it, as the manifest row
/// spells the claim.
pub const FUNCTION: toyos_manifest::device::PciMatch =
    toyos_manifest::device::PciMatch::new([0x04, 0x03, 0x00], None);

/// The register window: §3.3's registers are memory BAR 0.
const REGISTERS: u32 = 0;

/// The two mappings the controller's windows point into, held by both
/// halves for as long as either drives it.
struct Mappings {
    _bar: SharedMemory,
    _region: DmaRegion,
}

/// The stream: the mix thread's, which starts and stops it.
pub struct Hda {
    dev: PciDev,
    stream: Stream<Bar>,
    pcm: Window,
    underruns: u32,
    _held: Arc<Mappings>,
}

/// The codec's master: the control thread's, which sets and reads it back.
pub struct MasterControl {
    link: Link<Bar, Grant>,
    master: Master,
    setting: Setting,
    _held: Arc<Mappings>,
}

/// What a claim of the HDA function came to.
pub struct Claimed {
    pub hda: Hda,
    pub rate: u32,
    pub channels: u8,
    /// `None` where the path has no master, which soundserver names and serves
    /// as unavailable.
    pub master: Option<MasterControl>,
}

/// Why this machine's HDA function cannot carry audio. Each is a line
/// soundserver prints before falling back to the null sink.
pub enum Refusal {
    Kernel(KernelRefused),
    /// The description gives BAR 0 no window.
    NoRegisters,
    Controller(controller::Refusal),
    /// The codec link refused an answer during the walk or the configuration.
    Link(LinkFault),
    /// The codecs reach no output.
    Choice(Unchosen),
    /// The converter behind the chosen pin offers none of the device rates, at
    /// 16 bits; what it does offer, where it has a PCM word at all.
    Rate(Option<PcmCaps>),
}

impl From<KernelRefused> for Refusal {
    fn from(why: KernelRefused) -> Self {
        Self::Kernel(why)
    }
}

impl From<controller::Refusal> for Refusal {
    fn from(why: controller::Refusal) -> Self {
        Self::Controller(why)
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
            Self::Kernel(why) => write!(f, "{why}"),
            Self::NoRegisters => write!(f, "the claim gives its register BAR no window"),
            Self::Controller(why) => write!(f, "the controller refused: {why}"),
            Self::Link(why) => write!(f, "the codec link refused: {why}"),
            Self::Choice(Unchosen(found)) => {
                write!(f, "its codecs reach no output")?;
                for walk in found.iter().filter_map(|walk| walk.as_ref().err()) {
                    write!(f, ": ")?;
                    path_error(f, walk)?;
                }
                Ok(())
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

struct NoMasterLine(NoMaster);

impl core::fmt::Display for NoMasterLine {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            NoMaster::NoGain => write!(f, "no amplifier on the path has a gain range"),
            NoMaster::NoMute => write!(f, "neither an output pin nor the converter has a mute bit"),
        }
    }
}

/// Every codec `statests` names, walked over the link.
fn walk(link: &mut Link<Bar, Grant>, statests: u16) -> Vec<Codec> {
    let mut codecs = Vec::new();
    for entry in probe::enumerate(link, statests) {
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
    codecs
}

impl Hda {
    /// Bring the controller up, walk its codecs, choose the path that reaches
    /// an output, and configure it at the first device rate its converter
    /// offers, in §4.5.3's order: the descriptor, then the codec.
    ///
    /// Everything here runs before soundserver starts a thread.
    pub fn claim(dev: PciDev) -> Result<Claimed, Refusal> {
        let info = dev.describe().map_err(KernelRefused::on("the description"))?;
        let bytes = info.bar_bytes[REGISTERS as usize];
        if bytes == 0 {
            return Err(Refusal::NoRegisters);
        }
        say!("soundserver: hda {:02x}:{:02x}.{}", info.bus, info.dev, info.func);
        let (bar, bar_mapping) = Bar::map(&dev, REGISTERS, bytes)?;
        let reset = controller::reset(&bar, || std::thread::sleep(Duration::from_micros(CODEC_DETECT_MICROS)))?;
        say!("soundserver: hda gcap={:#06x} statests={:#06x}", reset.gcap.0, reset.statests);

        let (grant, region) = Grant::alloc(&dev, GRANT_BYTES as u64)?;
        grant.window().zero();
        let held = Arc::new(Mappings { _bar: bar_mapping, _region: region });

        let mut link = Link::open(bar, grant, &reset)?;
        let mut walks = vec![walk(&mut link, reset.statests)];
        if let Some(fault) = link.refused() {
            return Err(Refusal::Link(fault));
        }
        let (_, path) = toyos_hda::choose(&walks).map_err(Refusal::Choice)?;
        let codecs = walks.swap_remove(0);

        let rate = config::rate(&codecs, &path, &toyos_mixer::DEVICE_RATES).map_err(Refusal::Rate)?;
        let (format, channels) =
            config::format(&codecs, &path, rate).expect("a rate the converter offers has a format");
        say!(
            "soundserver: hda codec{} group {:#04x} converter {:#04x} -> pin {:#04x} ({}), \
             headphone {}, format {:#06x} ({} Hz {} ch {}-bit)",
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

        let stream = Stream::open(bar, &grant, &reset, format)?;
        let verbs = config::verbs(&codecs, &path, format, controller::STREAM_TAG)
            .expect("the path names a codec this walk produced");
        let sent = verbs.len();
        for verb in verbs {
            link.send(verb).map_err(Refusal::Link)?;
        }
        say!("soundserver: hda path configured in {sent} verbs, stream tag {}", controller::STREAM_TAG);

        let master = match config::master(&path) {
            Ok(master) => Some(MasterControl { link, master, setting: Setting::TOP, _held: held.clone() }),
            Err(why) => {
                say!("soundserver: hda path has no master ({}); volume answers unavailable", NoMasterLine(why));
                None
            }
        };
        let hda = Hda { dev, stream, pcm: grant.window(), underruns: 0, _held: held };
        Ok(Claimed { hda, rate, channels, master })
    }

    pub fn dev(&self) -> &PciDev {
        &self.dev
    }

    /// Where period `idx`'s samples go.
    pub fn buffer(&self, idx: usize) -> Window {
        self.pcm.sub(Stream::<Bar>::period(idx), PERIOD_BYTES)
    }

    /// The periods played since the last call, on the claim's record.
    ///
    /// The stream raises one message per status it clears, for the first
    /// period to complete after the clear, so the record's newest is never
    /// earlier in play order than the first period of this mask: that one is
    /// clocked by it, never early, and the rest wait for the next record's.
    pub fn completion(&mut self) -> Option<Completion> {
        let record = match self.dev.irq() {
            Ok(record) => record,
            Err(SyscallError::WouldBlock) => return None,
            Err(why) => panic!("soundserver: the HDA claim's interrupt record: {why:?}"),
        };
        let played = self
            .stream
            .played()
            .unwrap_or_else(|why| panic!("soundserver: hda cannot be driven on — {why}"));
        if played.underran {
            self.underruns += 1;
            if self.underruns == 1 {
                say!("soundserver: hda stream underran (FIFOE); counted from here and not said again");
            }
        }
        (played.mask != 0).then_some(Completion {
            mask: played.mask,
            first_nanos: record.first_nanos,
            last_nanos: record.last_nanos,
            clocked: 1,
        })
    }

    /// Start the engine, on the edge only: the descriptor list is cyclic, so
    /// a period costs this driver a record and no register write.
    pub fn start(&mut self) {
        self.stream
            .start()
            .unwrap_or_else(|why| panic!("soundserver: hda could not start its stream: {why}"));
    }

    pub fn stop(&mut self) {
        self.stream
            .stop()
            .unwrap_or_else(|why| panic!("soundserver: hda could not stop its stream: {why}"));
    }
}

/// What the master answers a request with: the setting read back from the
/// codec after it was set, never the intent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Answer {
    pub setting: Setting,
    pub bottom: u8,
}

impl MasterControl {
    /// Move the level by `delta` positions; a step while muted unmutes.
    pub fn step(&mut self, delta: i32) -> Option<Answer> {
        self.apply(self.master.step(self.setting, delta))
    }

    /// Mute, or unmute at the level it had.
    pub fn toggle_mute(&mut self) -> Option<Answer> {
        self.apply(Setting { muted: !self.setting.muted, ..self.setting })
    }

    /// Set `setting` and read it back. `None`, named, where the link refused
    /// or the answers name no setting.
    fn apply(&mut self, setting: Setting) -> Option<Answer> {
        for verb in self.master.set(setting) {
            if let Err(why) = self.link.send(verb) {
                say!("soundserver: hda master set refused ({:#010x}): {why}", verb.raw());
                return None;
            }
        }
        let mut answers = Vec::new();
        for verb in self.master.read() {
            match self.link.send(verb).map(toyos_hda::verb::Response::new) {
                Ok(Some(answer)) => answers.push(answer),
                Ok(None) => {
                    say!("soundserver: hda master readback answered all ones");
                    return None;
                }
                Err(why) => {
                    say!("soundserver: hda master readback refused: {why}");
                    return None;
                }
            }
        }
        let Some(read) = self.master.decode(&answers) else {
            say!("soundserver: hda master read back as no setting ({answers:?})");
            return None;
        };
        self.setting = read;
        Some(Answer { setting: read, bottom: self.master.bottom() })
    }
}
