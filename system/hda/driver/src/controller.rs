//! The controller: its reset, the codec link over CORB and RIRB in one grant,
//! and one output stream cycling a ring the device reaches.
//!
//! Every `§` is a section of the *Intel High Definition Audio Specification*,
//! Revision 1.0a.
//!
//! # The controller is not trusted
//!
//! Every word it writes back — a ring pointer, a response, a link position, a
//! status — is input from outside the driver, and one that no conforming
//! controller writes is a refusal by name. **A malformed answer ends the
//! link**: one response attributed to the wrong codec, or one too many, shifts
//! every answer after it onto the wrong verb, so a [`Link`] that has refused
//! one answers nothing again. A codec that answers nothing at all is that
//! codec's silence and no more.
//!
//! # Waits
//!
//! A register a step waits on is polled a bounded number of times
//! ([`POLLS`]), never against a wall clock: a clock keeps running while a guest
//! is descheduled, and a duration would measure the host's scheduler. The one
//! fixed delay is §4.3's, which no register reports the end of.

use crate::stream;
use crate::verb::Verb;
use toyos_device_memory::{DmaBuffers, Registers};

/// The pipeline: eight periods of 128 frames of 16-bit stereo.
pub const PERIODS: usize = 8;
pub const PERIOD_BYTES: usize = 512;

/// The tag the stream carries on the link, and the one the codec's converter
/// is told (§3.3.35: tag 0 is reserved as unused).
pub const STREAM_TAG: u8 = 1;

/// The grant: CORB, RIRB and the descriptor list, each on a 128-byte boundary
/// (§3.6.2, §3.6.4, §3.6.5). The periods are the ring soundserver lends.
const CORB: usize = 0x0000;
const RIRB: usize = 0x0400;
const BDL: usize = 0x0C00;
pub const GRANT_BYTES: usize = 0x1000;

/// §4.5.1: a buffer starts on a 128-byte boundary.
const BUFFER_ALIGN: u64 = 128;

const _: () = {
    assert!(CORB + 256 * 4 <= RIRB);
    assert!(RIRB + 256 * 8 <= BDL);
    assert!(BDL + PERIODS * 16 <= GRANT_BYTES);
    assert!(PERIOD_BYTES.is_multiple_of(BUFFER_ALIGN as usize));
};

/// How many times a register is read while a step waits on it.
///
/// Policy, not physics: the specification bounds only a stream's stop (40 µs,
/// §4.5.4), and a verb's answer comes in the frame after it (§4.4.2, about
/// 21 µs). A million reads is far past either on any controller that answers.
pub const POLLS: u32 = 1 << 20;

/// §4.3: codecs request their addresses within 25 frames of the link leaving
/// reset, and nothing reports when the last one has.
pub const CODEC_DETECT_MICROS: u64 = 521;

// §3.3, the controller's registers.
const GCAP: usize = 0x00;
const GCTL: usize = 0x08;
const WAKEEN: usize = 0x0C;
const STATESTS: usize = 0x0E;
const INTCTL: usize = 0x20;
const CORBLBASE: usize = 0x40;
const CORBUBASE: usize = 0x44;
const CORBWP: usize = 0x48;
const CORBRP: usize = 0x4A;
const CORBCTL: usize = 0x4C;
const CORBSIZE: usize = 0x4E;
const RIRBLBASE: usize = 0x50;
const RIRBUBASE: usize = 0x54;
const RIRBWP: usize = 0x58;
const RINTCNT: usize = 0x5A;
const RIRBCTL: usize = 0x5C;
const RIRBSTS: usize = 0x5D;
const RIRBSIZE: usize = 0x5E;
/// Where the first stream descriptor is, and how far apart they are (§3.3).
const SD_BASE: usize = 0x80;
const SD_STRIDE: usize = 0x20;

const GCTL_CRST: u32 = 1 << 0;
const GCTL_UNSOL: u32 = 1 << 8;
/// STATESTS' fifteen flags; bit 15 is reserved (§3.3.9).
const STATESTS_FLAGS: u16 = 0x7FFF;
const WAKEEN_FLAGS: u16 = 0x7FFF;
const INTCTL_GIE: u32 = 1 << 31;
const CORBRP_RST: u16 = 1 << 15;
const CORBCTL_RUN: u8 = 1 << 1;
const RIRBWP_RST: u16 = 1 << 15;
const RIRBCTL_DMAEN: u8 = 1 << 1;
const RIRBCTL_RINTCTL: u8 = 1 << 0;
const RIRBSTS_OIS: u8 = 1 << 2;
const RIRBSTS_RINTFL: u8 = 1 << 0;
/// A ring pointer's field: the low byte (§3.3.20, §3.3.27).
const POINTER: u16 = 0x00FF;
/// RIRB `Resp_Ex` (§4.4.2.1): the codec it came from and whether it was
/// unsolicited.
const RESP_CODEC: u32 = 0xF;
const RESP_UNSOL: u32 = 1 << 4;

// §3.3.35–§3.3.43, relative to one descriptor.
const SD_CTL: usize = 0x00;
const SD_CTL_STREAM: usize = 0x02;
const SD_STS: usize = 0x03;
const SD_LPIB: usize = 0x04;
const SD_CBL: usize = 0x08;
const SD_LVI: usize = 0x0C;
const SD_FMT: usize = 0x12;
const SD_BDPL: usize = 0x18;
const SD_BDPU: usize = 0x1C;

const SD_CTL_SRST: u8 = 1 << 0;
const SD_CTL_RUN: u8 = 1 << 1;
const SD_CTL_IOCE: u8 = 1 << 2;
const SD_CTL_FEIE: u8 = 1 << 3;
const SD_CTL_DEIE: u8 = 1 << 4;
/// The defined bits of `SDnCTL`'s first byte; the rest are preserved.
const SD_CTL_BITS: u8 = 0x1F;
const SD_STS_BCIS: u8 = 1 << 2;
const SD_STS_FIFOE: u8 = 1 << 3;
const SD_STS_DESE: u8 = 1 << 4;
const SD_STS_CLEAR: u8 = SD_STS_BCIS | SD_STS_FIFOE | SD_STS_DESE;
/// `SDnFMT`'s reserved bits 15 and 7 (§3.3.41).
const SD_FMT_RESERVED: u16 = 0x8080;

/// GCAP (§3.3.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Gcap(pub u16);

impl Gcap {
    pub fn output_streams(self) -> usize {
        (self.0 >> 12 & 0xF) as usize
    }

    pub fn input_streams(self) -> usize {
        (self.0 >> 8 & 0xF) as usize
    }

    pub fn bidirectional_streams(self) -> usize {
        (self.0 >> 3 & 0x1F) as usize
    }

    pub fn wide(self) -> bool {
        self.0 & 1 != 0
    }

    fn descriptors(self) -> usize {
        self.input_streams() + self.output_streams() + self.bidirectional_streams()
    }
}

/// Which ring a refusal is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ring {
    Corb,
    Rirb,
}

/// Why the controller is not brought up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// The register window is shorter than the registers this driver reaches.
    Window { bytes: usize, needs: usize },
    /// GCAP reads all ones: nothing answers at the window.
    AllOnes,
    /// A run bit §3.3.7 has cleared before reset never read back 0.
    StillRunning,
    /// CRST never read back 0 after a 0 was written, or 1 after a 1.
    NeverEnteredReset { gctl: u32 },
    NeverLeftReset { gctl: u32 },
    /// STATESTS reports no codec on the link.
    NoCodec,
    /// The size capability names no size §3.3.24 defines, or the size read
    /// back is not the one written.
    RingSize { ring: Ring, size: u8 },
    /// CORBRPRST never read back 1 after the reset, or 0 after the clear
    /// (§3.3.21).
    ReadPointerReset { set: bool },
    /// The grant lies above 4 GiB on a controller without 64OK (§3.3.2).
    Beyond32Bits { device_addr: u64 },
    /// GCAP reports no output stream descriptor.
    NoOutputStream { gcap: Gcap },
    /// The lent ring is where §4.5.1 puts no buffer: off a 128-byte boundary.
    RingUnaligned { device_addr: u64 },
    /// The output descriptor's SRST never read back 1, or 0 (§3.3.35).
    StreamReset { entered: bool },
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Window { bytes, needs } => {
                write!(f, "its register window is {bytes:#x} bytes, and this driver reaches {needs:#x}")
            }
            Self::AllOnes => write!(f, "its register window reads all ones"),
            Self::StillRunning => write!(f, "a DMA engine never stopped before the controller reset"),
            Self::NeverEnteredReset { gctl } => write!(f, "it never entered reset (GCTL {gctl:#010x})"),
            Self::NeverLeftReset { gctl } => write!(f, "it never left reset (GCTL {gctl:#010x})"),
            Self::NoCodec => write!(f, "no codec is on its link (STATESTS 0)"),
            Self::RingSize { ring, size } => write!(f, "its {ring:?} size register {size:#04x} offers no size this driver sets"),
            Self::ReadPointerReset { set: true } => write!(f, "its CORB read pointer reset never read back 1"),
            Self::ReadPointerReset { set: false } => write!(f, "its CORB read pointer reset never read back 0"),
            Self::Beyond32Bits { device_addr } => {
                write!(f, "it addresses 32 bits and the grant is at {device_addr:#x}")
            }
            Self::NoOutputStream { gcap } => write!(f, "it has no output stream (GCAP {:#06x})", gcap.0),
            Self::RingUnaligned { device_addr } => write!(f, "the lent ring at {device_addr:#x} is off a 128-byte boundary"),
            Self::StreamReset { entered: true } => write!(f, "its output stream never entered reset"),
            Self::StreamReset { entered: false } => write!(f, "its output stream never left reset"),
        }
    }
}

/// Why a verb got no answer the link can vouch for. Every one but
/// [`LinkFault::Silent`] ends the link.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LinkFault {
    /// RIRBWP never moved after the verb went out: an absent codec's answer
    /// to anything. Nothing arrived, so the rings are still in step; an answer
    /// arriving after all is [`LinkFault::Extra`] on the next verb.
    Silent { verb: u32 },
    /// RIRBWP names an entry past the ring.
    PointerOutsideRing { written: u8, entries: u16 },
    /// More than the one response the one verb in flight is owed.
    Extra { responses: u16 },
    /// A response from another codec than the one asked.
    WrongCodec { asked: u8, answered: u8 },
    /// An unsolicited response, which GCTL told the controller to drop.
    Unsolicited,
    /// RIRBOIS: the controller dropped responses.
    Overrun,
}

impl core::fmt::Display for LinkFault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Silent { verb } => write!(f, "verb {verb:#010x} was never answered"),
            Self::PointerOutsideRing { written, entries } => {
                write!(f, "RIRBWP {written} is outside its {entries}-entry ring")
            }
            Self::Extra { responses } => write!(f, "one verb was answered {responses} times"),
            Self::WrongCodec { asked, answered } => {
                write!(f, "codec {answered} answered a verb sent to codec {asked}")
            }
            Self::Unsolicited => write!(f, "an unsolicited response arrived, which the controller was told to drop"),
            Self::Overrun => write!(f, "the controller overran the RIRB"),
        }
    }
}

/// Why a running stream is not driven on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StreamFault {
    /// DESE: the engine stopped on a descriptor fetch (§3.3.36).
    Descriptor,
    /// LPIB names no byte of the cyclic buffer (§3.3.37).
    PositionOutsideBuffer { position: u32 },
    /// RUN never read back 0 after a stop (§4.5.4).
    NeverStopped,
}

impl core::fmt::Display for StreamFault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Descriptor => write!(f, "the stream stopped on a descriptor error"),
            Self::PositionOutsideBuffer { position } => {
                write!(f, "the stream's link position {position:#x} is outside its buffer")
            }
            Self::NeverStopped => write!(f, "the stream never stopped"),
        }
    }
}

/// What the controller's reset found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reset {
    pub gcap: Gcap,
    /// Every codec address that requested one, one bit each (§4.3).
    pub statests: u16,
}

fn settles(mut ready: impl FnMut() -> bool) -> bool {
    for _ in 0..POLLS {
        if ready() {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Put the controller through reset and bring its link up (§4.2.2, §4.3).
///
/// `codec_detect` waits [`CODEC_DETECT_MICROS`].
pub fn reset<R: Registers>(regs: &R, codec_detect: impl FnOnce()) -> Result<Reset, Refusal> {
    if regs.bytes() < SD_BASE {
        return Err(Refusal::Window { bytes: regs.bytes(), needs: SD_BASE });
    }
    let gcap = Gcap(regs.read16(GCAP));
    if gcap.0 == u16::MAX {
        return Err(Refusal::AllOnes);
    }
    let needs = SD_BASE + gcap.descriptors() * SD_STRIDE;
    if regs.bytes() < needs {
        return Err(Refusal::Window { bytes: regs.bytes(), needs });
    }

    // §3.3.7: every run bit is verified 0 before CRST is asserted. In reset
    // they read 0 already.
    regs.write8(CORBCTL, regs.read8(CORBCTL) & !CORBCTL_RUN);
    regs.write8(RIRBCTL, regs.read8(RIRBCTL) & !RIRBCTL_DMAEN);
    for descriptor in 0..gcap.descriptors() {
        let ctl = SD_BASE + descriptor * SD_STRIDE + SD_CTL;
        regs.write8(ctl, regs.read8(ctl) & !SD_CTL_RUN);
    }
    let stopped = || {
        regs.read8(CORBCTL) & CORBCTL_RUN == 0
            && regs.read8(RIRBCTL) & RIRBCTL_DMAEN == 0
            && (0..gcap.descriptors())
                .all(|d| regs.read8(SD_BASE + d * SD_STRIDE + SD_CTL) & SD_CTL_RUN == 0)
    };
    if !settles(stopped) {
        return Err(Refusal::StillRunning);
    }

    regs.write32(GCTL, regs.read32(GCTL) & !(GCTL_CRST | GCTL_UNSOL));
    if !settles(|| regs.read32(GCTL) & GCTL_CRST == 0) {
        return Err(Refusal::NeverEnteredReset { gctl: regs.read32(GCTL) });
    }
    regs.write32(GCTL, regs.read32(GCTL) | GCTL_CRST);
    if !settles(|| regs.read32(GCTL) & GCTL_CRST != 0) {
        return Err(Refusal::NeverLeftReset { gctl: regs.read32(GCTL) });
    }
    codec_detect();

    let statests = regs.read16(STATESTS) & STATESTS_FLAGS;
    regs.write16(STATESTS, statests);
    // §3.3.8: nothing may be assumed about it; this driver takes no wake.
    regs.write16(WAKEEN, regs.read16(WAKEEN) & !WAKEEN_FLAGS);
    regs.write32(INTCTL, 0);
    if statests == 0 {
        return Err(Refusal::NoCodec);
    }
    Ok(Reset { gcap, statests })
}

/// The largest ring §3.3.24/§3.3.31 define that the capability offers, as
/// its size code and its entries.
fn ring_size(cap: u8) -> Option<(u8, u16)> {
    [(0b10, 256, 1 << 6), (0b01, 16, 1 << 5), (0b00, 2, 1 << 4)]
        .into_iter()
        .find(|&(_, _, bit)| cap & bit != 0)
        .map(|(code, entries, _)| (code, entries))
}

fn set_ring_size<R: Registers>(regs: &R, at: usize, ring: Ring) -> Result<u16, Refusal> {
    let size = regs.read8(at);
    let (code, entries) = ring_size(size).ok_or(Refusal::RingSize { ring, size })?;
    regs.write8(at, size & !0b11 | code);
    let read = regs.read8(at);
    if read & 0b11 != code {
        return Err(Refusal::RingSize { ring, size: read });
    }
    Ok(entries)
}

fn base<R: Registers>(regs: &R, low: usize, high: usize, device_addr: u64, gcap: Gcap) -> Result<(), Refusal> {
    if !gcap.wide() && device_addr >> 32 != 0 {
        return Err(Refusal::Beyond32Bits { device_addr });
    }
    regs.write32(low, device_addr as u32);
    regs.write32(high, (device_addr >> 32) as u32);
    Ok(())
}

/// The codec link: one verb in flight at a time, over CORB and RIRB (§4.4).
pub struct Link<R: Registers, M: DmaBuffers> {
    regs: R,
    mem: M,
    corb_entries: u16,
    rirb_entries: u16,
    /// The last CORB entry written, and the last RIRB entry read: §4.4.2 has
    /// the RIRB's read pointer kept by software alone.
    corb_wp: u16,
    rirb_rp: u16,
    refused: Option<LinkFault>,
}

impl<R: Registers, M: DmaBuffers> Link<R, M> {
    /// §4.4.1.3 and §4.4.2.2, on a controller [`reset`] brought up.
    ///
    /// # Panics
    /// If the grant is shorter than [`GRANT_BYTES`]: the caller's own mistake.
    pub fn open(regs: R, mem: M, reset: &Reset) -> Result<Self, Refusal> {
        assert!(mem.bytes() >= GRANT_BYTES, "hda: a grant of {:#x} bytes holds no layout", mem.bytes());
        let corb_entries = set_ring_size(&regs, CORBSIZE, Ring::Corb)?;
        let rirb_entries = set_ring_size(&regs, RIRBSIZE, Ring::Rirb)?;
        base(&regs, CORBLBASE, CORBUBASE, mem.device_addr(CORB), reset.gcap)?;
        base(&regs, RIRBLBASE, RIRBUBASE, mem.device_addr(RIRB), reset.gcap)?;

        regs.write16(CORBRP, regs.read16(CORBRP) | CORBRP_RST);
        if !settles(|| regs.read16(CORBRP) & CORBRP_RST != 0) {
            return Err(Refusal::ReadPointerReset { set: true });
        }
        regs.write16(CORBRP, regs.read16(CORBRP) & !CORBRP_RST);
        if !settles(|| regs.read16(CORBRP) & CORBRP_RST == 0) {
            return Err(Refusal::ReadPointerReset { set: false });
        }
        regs.write16(CORBWP, regs.read16(CORBWP) & !POINTER);
        regs.write16(RIRBWP, RIRBWP_RST);
        // RINTFL after every response (§3.3.28, §3.3.29), cleared as each is
        // read (§3.3.30): a controller may hold the RIRB at a reached count
        // until its flag is acknowledged. INTCTL's CIE stays clear, so the flag
        // raises no interrupt (§3.3.13); a verb's answer is polled for.
        regs.write16(RINTCNT, regs.read16(RINTCNT) & 0xFF00 | 1);
        regs.write8(RIRBSTS, RIRBSTS_OIS | RIRBSTS_RINTFL);

        regs.write8(RIRBCTL, regs.read8(RIRBCTL) & !0b111 | RIRBCTL_DMAEN | RIRBCTL_RINTCTL);
        regs.write8(CORBCTL, regs.read8(CORBCTL) & !0b11 | CORBCTL_RUN);
        if !settles(|| regs.read8(CORBCTL) & CORBCTL_RUN != 0 && regs.read8(RIRBCTL) & RIRBCTL_DMAEN != 0) {
            return Err(Refusal::StillRunning);
        }
        Ok(Self { regs, mem, corb_entries, rirb_entries, corb_wp: 0, rirb_rp: 0, refused: None })
    }

    /// The fault that ended the link, if one has.
    pub fn refused(&self) -> Option<LinkFault> {
        self.refused
    }

    /// One verb, and the codec's 32-bit answer.
    pub fn send(&mut self, verb: Verb) -> Result<u32, LinkFault> {
        if let Some(fault) = self.refused {
            return Err(fault);
        }
        let answer = self.exchange(verb);
        match answer {
            Err(LinkFault::Silent { .. }) | Ok(_) => {}
            Err(fault) => self.refused = Some(fault),
        }
        answer
    }

    fn exchange(&mut self, verb: Verb) -> Result<u32, LinkFault> {
        // §4.4.1.4: the entry after the write pointer, then the pointer.
        let wp = (self.corb_wp + 1) % self.corb_entries;
        self.mem.write32(CORB + wp as usize * 4, verb.raw());
        self.mem.publish();
        self.regs.write16(CORBWP, self.regs.read16(CORBWP) & !POINTER | wp);
        self.corb_wp = wp;

        let regs = &self.regs;
        let rp = self.rirb_rp;
        let mut written = rp;
        if !settles(|| {
            written = regs.read16(RIRBWP) & POINTER;
            written != rp
        }) {
            return Err(LinkFault::Silent { verb: verb.raw() });
        }
        if written >= self.rirb_entries {
            return Err(LinkFault::PointerOutsideRing { written: written as u8, entries: self.rirb_entries });
        }
        let responses = (written + self.rirb_entries - rp) % self.rirb_entries;
        if responses != 1 {
            return Err(LinkFault::Extra { responses });
        }
        if regs.read8(RIRBSTS) & RIRBSTS_OIS != 0 {
            return Err(LinkFault::Overrun);
        }
        self.mem.observe();
        let at = RIRB + written as usize * 8;
        let (response, extended) = (self.mem.read32(at), self.mem.read32(at + 4));
        self.rirb_rp = written;
        regs.write8(RIRBSTS, RIRBSTS_RINTFL);
        if extended & RESP_UNSOL != 0 {
            return Err(LinkFault::Unsolicited);
        }
        let asked = (verb.raw() >> 28) as u8;
        let answered = (extended & RESP_CODEC) as u8;
        if answered != asked {
            return Err(LinkFault::WrongCodec { asked, answered });
        }
        Ok(response)
    }
}

impl<R: Registers, M: DmaBuffers> crate::probe::Verbs for Link<R, M> {
    fn get(
        &mut self,
        codec: crate::verb::Address,
        node: crate::verb::Node,
        verb: u16,
        payload: u8,
    ) -> Option<crate::verb::Response> {
        self.send(Verb::short(codec, node, verb, payload)).ok().and_then(crate::verb::Response::new)
    }
}

/// What a stream interrupt found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Played {
    /// The periods played since the last look, one bit each.
    pub mask: u32,
    /// FIFOE: the engine ran out of data with buffers left to send (§3.3.36).
    pub underran: bool,
}

/// The first output stream descriptor, cycling the grant's periods.
pub struct Stream<R: Registers> {
    regs: R,
    /// The descriptor's offset in the window.
    at: usize,
    /// The period the engine was in at the last look.
    last: usize,
    running: bool,
}

impl<R: Registers> Stream<R> {
    /// §4.5.3's descriptor half: reset it, point it at the cyclic buffer of
    /// [`PERIODS`] periods of [`PERIOD_BYTES`] the device reaches at `ring`,
    /// and arm its interrupt. The codec is told the format and [`STREAM_TAG`]
    /// after this and before [`Self::start`].
    ///
    /// # Panics
    /// If the grant is shorter than [`GRANT_BYTES`]: the caller's own mistake.
    pub fn open(regs: R, mem: &impl DmaBuffers, ring: u64, reset: &Reset, format: u16) -> Result<Self, Refusal> {
        assert!(mem.bytes() >= GRANT_BYTES, "hda: a grant of {:#x} bytes holds no layout", mem.bytes());
        let gcap = reset.gcap;
        if gcap.output_streams() == 0 {
            return Err(Refusal::NoOutputStream { gcap });
        }
        if !ring.is_multiple_of(BUFFER_ALIGN) {
            return Err(Refusal::RingUnaligned { device_addr: ring });
        }
        let end = ring + (PERIODS * PERIOD_BYTES) as u64 - 1;
        if !gcap.wide() && end >> 32 != 0 {
            return Err(Refusal::Beyond32Bits { device_addr: ring });
        }
        // §3.3.14: input descriptors are numbered first.
        let index = gcap.input_streams();
        let at = SD_BASE + index * SD_STRIDE;

        let ctl = at + SD_CTL;
        regs.write8(ctl, regs.read8(ctl) | SD_CTL_SRST);
        if !settles(|| regs.read8(ctl) & SD_CTL_SRST != 0) {
            return Err(Refusal::StreamReset { entered: true });
        }
        regs.write8(ctl, regs.read8(ctl) & !SD_CTL_SRST);
        if !settles(|| regs.read8(ctl) & SD_CTL_SRST == 0) {
            return Err(Refusal::StreamReset { entered: false });
        }

        let entries = stream::build_bdl(ring, PERIOD_BYTES as u32, PERIODS)
            .expect("hda: a ring the device reaches builds a descriptor list");
        for (i, entry) in entries.iter().enumerate() {
            let slot = BDL + i * 16;
            mem.write64(slot, entry.address);
            mem.write32(slot + 8, entry.length);
            mem.write32(slot + 12, u32::from(entry.interrupt_on_completion));
        }
        mem.publish();

        base(&regs, at + SD_BDPL, at + SD_BDPU, mem.device_addr(BDL), gcap)?;
        let cbl = stream::cyclic_length(PERIOD_BYTES as u32, PERIODS).expect("fits a u32");
        regs.write32(at + SD_CBL, cbl);
        let lvi = stream::last_valid_index(PERIODS).expect("a ring of eight");
        regs.write16(at + SD_LVI, regs.read16(at + SD_LVI) & 0xFF00 | u16::from(lvi));
        let fmt = regs.read16(at + SD_FMT);
        regs.write16(at + SD_FMT, fmt & SD_FMT_RESERVED | format & !SD_FMT_RESERVED);
        // Tag in 23:20; striping off and traffic priority 0, which §3.3.35
        // warns may make the stream's traffic unsnooped.
        regs.write8(at + SD_CTL_STREAM, STREAM_TAG << 4);
        regs.write8(ctl, regs.read8(ctl) & !SD_CTL_BITS | SD_CTL_IOCE | SD_CTL_FEIE | SD_CTL_DEIE);
        regs.write8(at + SD_STS, SD_STS_CLEAR);
        regs.write32(INTCTL, INTCTL_GIE | 1 << index);
        Ok(Self { regs, at, last: 0, running: false })
    }

    pub fn running(&self) -> bool {
        self.running
    }

    /// Set RUN; nothing when it is set.
    ///
    /// §4.5.5 resumes where the engine stopped, so the period it is in is read
    /// rather than assumed, and a status left from before the stop is cleared.
    pub fn start(&mut self) -> Result<(), StreamFault> {
        if self.running {
            return Ok(());
        }
        let position = self.regs.read32(self.at + SD_LPIB);
        self.last = stream::period_at(position, PERIOD_BYTES as u32, PERIODS)
            .ok_or(StreamFault::PositionOutsideBuffer { position })?;
        self.regs.write8(self.at + SD_STS, SD_STS_CLEAR);
        let ctl = self.at + SD_CTL;
        self.regs.write8(ctl, self.regs.read8(ctl) | SD_CTL_RUN);
        self.running = true;
        Ok(())
    }

    /// Clear RUN and wait for it to read 0 (§4.5.4), so a start after it is
    /// one write; nothing when it is clear.
    pub fn stop(&mut self) -> Result<(), StreamFault> {
        if !self.running {
            return Ok(());
        }
        let (regs, ctl) = (&self.regs, self.at + SD_CTL);
        regs.write8(ctl, regs.read8(ctl) & !SD_CTL_RUN);
        if !settles(|| regs.read8(ctl) & SD_CTL_RUN == 0) {
            return Err(StreamFault::NeverStopped);
        }
        self.running = false;
        Ok(())
    }

    /// The periods played since the last look, on an interrupt.
    ///
    /// The position is read before the status is cleared, so a period that
    /// completes between the two is counted at the next look and never stamped
    /// with an interrupt older than itself; the one after it raises the next.
    pub fn played(&mut self) -> Result<Played, StreamFault> {
        let status = self.regs.read8(self.at + SD_STS);
        let position = self.regs.read32(self.at + SD_LPIB);
        self.regs.write8(self.at + SD_STS, status & SD_STS_CLEAR);
        if status & SD_STS_DESE != 0 {
            return Err(StreamFault::Descriptor);
        }
        let (mask, current) = stream::completed(self.last, position, PERIOD_BYTES as u32, PERIODS)
            .ok_or(StreamFault::PositionOutsideBuffer { position })?;
        self.last = current;
        Ok(Played { mask, underran: status & SD_STS_FIFOE != 0 })
    }
}

#[cfg(test)]
mod tests;
