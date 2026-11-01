//! The Intel HD Audio driver: one process per HDA function the kernel does not
//! drive, holding its claim (a `drives` row), and a client of soundserver's
//! output port.
//!
//! **The controller and every decision about its codecs are `toyos-hda`'s**:
//! the reset, CORB/RIRB, the stream, the walk, the path, the rate, the format
//! and the master. This file is what is left: the claim's two mappings, the
//! ring soundserver lends, the offer, and the loop that carries the stream's
//! interrupts to soundserver and soundserver's requests to the device.
//!
//! **A controller whose codecs reach no output offers nothing** and ends by
//! name: the GPU's display audio is one, and soundserver plays to the
//! controller that does reach one. A refusal after the offer was accepted ends
//! this program by name too; no conforming controller writes one.

use std::time::Duration;

use toyos::endow::{self, Endowments};
use toyos::ipc::{FrameRx, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::shm::SharedMemory;
use toyos::syscap::SysCap;
use toyos::{say, AsHandle, Connection, DmaRegion, OwnedHandle, PciDev};
use toyos_abi::syscall::SyscallError;
use toyos_hda::config::{self, Master, NoMaster, Setting};
use toyos_hda::controller::{
    self, Link, LinkFault, Stream, CODEC_DETECT_MICROS, GRANT_BYTES, PERIODS, PERIOD_BYTES,
};
use toyos_hda::graph::Codec;
use toyos_hda::path::PathError;
use toyos_hda::probe;
use toyos_hda::verb::Response;
use toyos_manifest::device::PciMatch;
use toyos_pci_claim::{Bar, Grant, KernelRefused};
use toyos_sound_output::{
    self as output, Engine, Offer, Played, ToDriver, MSG_MASTER, MSG_MASTER_UNAVAILABLE, MSG_OFFER, MSG_PLAYED,
    MSG_STREAM,
};

/// A multimedia audio device of subclass `03` (PCI Code and ID Assignment:
/// HD Audio), with whatever programming interface: `00` for the base register
/// model and `80` for one with vendor extensions, which the T14's has and this
/// driver does not use.
const FUNCTION: PciMatch = PciMatch::any_interface([0x04, 0x03], None);

/// The register window: §3.3's registers are memory BAR 0.
const REGISTERS: u32 = 0;

/// The widest frame soundserver sends: a master step's `i32`.
const MAX_REQUEST: usize = 4;

const TOKEN_CLAIM: u64 = 0;
const TOKEN_CONTROL: u64 = 1;
const TOKEN_STREAM: u64 = 2;

/// Why this function carries no audio. Each is the line this program ends on.
enum Refusal {
    Kernel(KernelRefused),
    NoRegisters,
    Controller(controller::Refusal),
    /// No width this function reaches is known.
    Width(controller::Contradicted),
    Link(LinkFault),
    NoOutput(PathError),
    /// The converter behind the chosen pin offers none of soundserver's rates
    /// at 16 bits; what it does offer, where it has a PCM word at all.
    Rate(Option<toyos_hda::caps::PcmCaps>),
    /// soundserver plays to another output.
    Declined,
    Soundserver(String),
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
        PathError::NoOutputPin { codecs } if codecs.is_empty() => write!(f, "no codec answered"),
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
            Self::Width(why) => write!(f, "{why}"),
            Self::Link(why) => write!(f, "the codec link refused: {why}"),
            Self::NoOutput(why) => {
                write!(f, "its codecs reach no output: ")?;
                path_error(f, why)
            }
            Self::Rate(None) => write!(f, "the converter has no PCM word"),
            Self::Rate(Some(pcm)) => {
                write!(f, "the converter offers none of {:?} Hz at {} bits; it offers", output::RATES, config::WIDTH)?;
                for rate in pcm.rates() {
                    write!(f, " {rate}")?;
                }
                write!(f, " Hz at {:?} bits", pcm.widths().collect::<Vec<_>>())
            }
            Self::Declined => write!(f, "soundserver plays to another output"),
            Self::Soundserver(why) => write!(f, "soundserver: {why}"),
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

/// The function, driven: what the loop needs and what it must keep alive.
struct Driver {
    at: String,
    dev: PciDev,
    link: Link<Bar, Grant>,
    stream: Stream<Bar>,
    master: Option<(Master, Setting)>,
    control: Connection,
    to_engine: Connection,
    /// The run soundserver's last start named, which every report carries.
    run: u32,
    underruns: u32,
    /// Held for their mappings' lives: the register window points into the
    /// first, and the link and descriptor list into the second. The lent ring
    /// stays alive while it is mapped; its handle is kept as the program's
    /// own reference to what the device reaches.
    _bar: SharedMemory,
    _region: DmaRegion,
    _ring: OwnedHandle,
}

fn main() {
    let dev = endow::claim::<PciDev>(&FUNCTION.to_string())
        .expect("hda: the drives row endows this process its function's claim");
    let info = match toyos_pci_claim::describe(&dev) {
        Ok(info) => info,
        Err(why) => {
            say!("hda: plays nothing: {why}");
            return;
        }
    };
    let at = format!("hda {:02x}:{:02x}.{}", info.bus, info.dev, info.func);
    let bar_bytes = info.bar_bytes[REGISTERS as usize];
    match Driver::up(dev, bar_bytes, &at) {
        Ok(driver) => driver.serve(),
        Err(why) => say!("{at}: plays nothing: {why}"),
    }
}

/// Every codec `statests` names, walked over the link.
fn walk(at: &str, link: &mut Link<Bar, Grant>, statests: u16) -> Vec<Codec> {
    let mut codecs = Vec::new();
    for entry in probe::enumerate(link, statests) {
        match entry {
            Ok(codec) => {
                say!(
                    "{at}: codec{} vendor={:04x} device={:04x}, {} function group(s)",
                    codec.address,
                    codec.vendor,
                    codec.device,
                    codec.groups.len()
                );
                codecs.push(codec);
            }
            Err((address, fault)) => say!("{at}: codec{address} answered nothing usable ({fault:?})"),
        }
    }
    codecs
}

impl Driver {
    /// Bring the controller up, walk its codecs and find the path that reaches
    /// an output; offer it, and on soundserver's acceptance point the stream at
    /// the ring it lends and configure the codec, in §4.5.3's order: the
    /// descriptor, then the codec.
    fn up(dev: PciDev, bar_bytes: u64, at: &str) -> Result<Self, Refusal> {
        if bar_bytes == 0 {
            return Err(Refusal::NoRegisters);
        }
        let (bar, bar_mapping) = Bar::map(&dev, REGISTERS, bar_bytes)?;
        // With bus mastering still off: the reset stops every DMA engine
        // before CRST (§3.3.7), so nothing firmware or a previous holder left
        // running reaches the grants laid out below.
        let reset = controller::reset(&bar, || std::thread::sleep(Duration::from_micros(CODEC_DETECT_MICROS)))?;
        say!("{at}: gcap={:#06x} statests={:#06x}", reset.gcap.0, reset.statests);
        let (vendor, device) = toyos_pci_claim::id(&dev)?;
        let bits = controller::address_bits(vendor, device, reset.gcap).map_err(Refusal::Width)?;
        toyos_pci_claim::limit(&dev, bits)?;

        let (grant, region) = Grant::alloc(&dev, GRANT_BYTES as u64)?;
        grant.window().zero();
        toyos_pci_claim::start(&dev)?;
        let mut link = Link::open(bar, grant, &reset)?;
        let codecs = walk(at, &mut link, reset.statests);
        if let Some(fault) = link.refused() {
            return Err(Refusal::Link(fault));
        }
        let path = toyos_hda::find_output_path(&codecs).map_err(Refusal::NoOutput)?;
        let rate = config::rate(&codecs, &path, &output::RATES).map_err(Refusal::Rate)?;
        let (format, channels) =
            config::format(&codecs, &path, rate).expect("a rate the converter offers has a format");
        say!(
            "{at}: codec{} group {:#04x} converter {:#04x} -> pin {:#04x} ({}), headphone {}, \
             format {:#06x} ({} Hz {} ch {}-bit)",
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
        let master = match config::master(&path) {
            Ok(master) => Some((master, Setting::TOP)),
            Err(why) => {
                say!("{at}: the path has no master ({}); volume answers unavailable", NoMasterLine(why));
                None
            }
        };

        let offer = Offer {
            rate,
            channels,
            periods: PERIODS as u8,
            period_bytes: PERIOD_BYTES as u32,
            master_bottom: master.as_ref().map(|(master, _)| master.bottom()),
        };
        let control = endow::service(output::PORT)
            .map_err(|why| Refusal::Soundserver(format!("no `{}` port ({why:?})", output::PORT)))?;
        let ring = offered(&control, &offer)?;
        let lent = toyos_pci_claim::lend(&dev, ring.as_handle())?;
        if lent.bytes < offer.ring_bytes() as u64 {
            return Err(Refusal::Soundserver(format!(
                "it lent a ring of {} bytes for {}",
                lent.bytes,
                offer.ring_bytes()
            )));
        }

        let stream = Stream::open(bar, &grant, lent.device_addr, &reset, format)?;
        let verbs = config::verbs(&codecs, &path, format, controller::STREAM_TAG)
            .expect("the path names a codec this walk produced");
        let sent = verbs.len();
        for verb in verbs {
            link.send(verb).map_err(Refusal::Link)?;
        }
        let to_engine = endow::service(output::PORT)
            .map_err(|why| Refusal::Soundserver(format!("no second `{}` connection ({why:?})", output::PORT)))?;
        to_engine
            .send_bytes(MSG_STREAM, &[])
            .map_err(|why| Refusal::Soundserver(format!("the stream connection was refused ({why:?})")))?;
        say!("{at}: path configured in {sent} verbs, stream tag {}; offered to soundserver", controller::STREAM_TAG);
        Ok(Self {
            at: at.to_string(),
            dev,
            link,
            stream,
            master,
            control,
            to_engine,
            run: 0,
            underruns: 0,
            _bar: bar_mapping,
            _region: region,
            _ring: ring,
        })
    }

    /// Carry the stream's interrupts to soundserver and soundserver's requests
    /// to the device, until soundserver hangs up.
    fn serve(mut self) {
        rt();
        let poller = Poller::new(4);
        let mut control_rx = FrameRx::<MAX_REQUEST>::new();
        let mut stream_rx = FrameRx::<MAX_REQUEST>::new();
        loop {
            poller.watch(&self.dev, READABLE, TOKEN_CLAIM);
            poller.watch(&self.control, READABLE, TOKEN_CONTROL);
            poller.watch(&self.to_engine, READABLE, TOKEN_STREAM);
            poller.wait(1, u64::MAX, |_| {});

            self.interrupts();
            while let Some((msg_type, payload)) = frame(&mut stream_rx, &self.to_engine, &self.at, "stream") {
                match Engine::decode(msg_type, &payload) {
                    Some(Engine::Start(run)) => {
                        self.run = run;
                        self.stream.start()
                    }
                    Some(Engine::Stop) => self.stream.stop(),
                    None => panic!("{}: soundserver sent stream frame {msg_type}, which this protocol has not", self.at),
                }
                .unwrap_or_else(|why| panic!("{}: cannot be driven on — {why}", self.at));
            }
            while let Some((msg_type, payload)) = frame(&mut control_rx, &self.control, &self.at, "control") {
                match ToDriver::decode(msg_type, &payload) {
                    Some(ToDriver::MasterStep(delta)) => {
                        let target = self.master.as_ref().map(|(master, setting)| master.step(*setting, delta));
                        self.answer(target);
                    }
                    Some(ToDriver::MasterMute) => {
                        let target = self.master.as_ref().map(|&(_, setting)| Setting { muted: !setting.muted, ..setting });
                        self.answer(target);
                    }
                    _ => panic!("{}: soundserver sent control frame {msg_type}, which this state has not", self.at),
                }
            }
        }
    }

    /// The periods the stream played, reported with when its interrupts said
    /// so.
    ///
    /// The stream raises one message per status it clears, for the first
    /// period to complete after the clear, so the record's newest is never
    /// earlier in play order than the first period of the mask: that one is
    /// clocked by it, and the rest wait for the next record's.
    fn interrupts(&mut self) {
        let record = match self.dev.irq() {
            Ok(record) => record,
            Err(SyscallError::WouldBlock) => return,
            Err(why) => panic!("{}: the claim's interrupt record: {why:?}", self.at),
        };
        let played = self.stream.played().unwrap_or_else(|why| panic!("{}: cannot be driven on — {why}", self.at));
        if played.underran {
            self.underruns += 1;
            if self.underruns == 1 {
                say!("{}: the stream underran (FIFOE); counted from here and not said again", self.at);
            }
        }
        if played.mask == 0 {
            return;
        }
        let report = Played {
            mask: played.mask,
            clocked: 1,
            first_nanos: record.first_nanos,
            last_nanos: record.last_nanos,
            run: self.run,
        };
        if let Err(why) = self.to_engine.send_bytes(MSG_PLAYED, &report.encode()) {
            soundserver_gone(&self.at, why);
        }
    }

    /// Set the master to `target` and answer what the codec read back.
    fn answer(&mut self, target: Option<Setting>) {
        let read = target.and_then(|target| self.set(target));
        let sent = match read {
            Some(read) => {
                let (master, setting) = self.master.as_mut().expect("a read master is one this path has");
                *setting = read;
                let answer = output::Master { position: read.position, bottom: master.bottom(), muted: read.muted };
                self.control.send_bytes(MSG_MASTER, &answer.encode())
            }
            None => self.control.send_bytes(MSG_MASTER_UNAVAILABLE, &[]),
        };
        if let Err(why) = sent {
            soundserver_gone(&self.at, why);
        }
    }

    /// Set `target` on the codec and read it back; `None`, named, where the
    /// link refused or the answers name no setting.
    fn set(&mut self, target: Setting) -> Option<Setting> {
        let (master, _) = self.master.as_ref()?;
        for verb in master.set(target) {
            if let Err(why) = self.link.send(verb) {
                say!("{}: the master set was refused ({:#010x}): {why}", self.at, verb.raw());
                return None;
            }
        }
        let mut answers = Vec::new();
        for verb in master.read() {
            match self.link.send(verb).map(Response::new) {
                Ok(Some(answer)) => answers.push(answer),
                Ok(None) => {
                    say!("{}: the master readback answered all ones", self.at);
                    return None;
                }
                Err(why) => {
                    say!("{}: the master readback was refused: {why}", self.at);
                    return None;
                }
            }
        }
        let read = master.decode(&answers);
        if read.is_none() {
            say!("{}: the master read back as no setting ({answers:?})", self.at);
        }
        read
    }
}

/// How long soundserver has to answer the offer: a hang ceiling, since it
/// answers in the control-thread pass that reads it. Past it the controller is
/// given back rather than held by a driver waiting on nothing.
const OFFER_ANSWER: Duration = Duration::from_secs(30);

/// Offer `offer` on `control` and take soundserver's answer: the ring it lends.
fn offered(control: &Connection, offer: &Offer) -> Result<OwnedHandle, Refusal> {
    let refused = |why: String| Refusal::Soundserver(why);
    control.send_bytes(MSG_OFFER, &offer.encode()).map_err(|why| refused(format!("the offer was refused ({why:?})")))?;
    let answered = toyos::poller::wait_one(control, READABLE, OFFER_ANSWER.as_nanos() as u64)
        .map_err(|why| refused(format!("its answer could not be waited for ({why:?})")))?;
    if !answered {
        return Err(refused(format!("it did not answer the offer within {OFFER_ANSWER:?}")));
    }
    let mut rx = FrameRx::<MAX_REQUEST>::new();
    let frame = rx.recv(control).map_err(|why| refused(format!("no answer to the offer ({why:?})")))?;
    match ToDriver::decode(frame.msg_type, rx.payload(frame.payload_len)) {
        Some(ToDriver::Accepted) => {
            let [ring] = rx
                .take_handles::<{ output::ACCEPTED_HANDLES }>()
                .ok_or_else(|| refused("its acceptance carried no ring".into()))?;
            Ok(ring)
        }
        Some(ToDriver::Refused) => Err(Refusal::Declined),
        _ => Err(refused(format!("it answered the offer with frame {}", frame.msg_type))),
    }
}

/// The next whole frame soundserver sent on `conn`, if one is there.
fn frame(rx: &mut FrameRx<MAX_REQUEST>, conn: &Connection, at: &str, which: &str) -> Option<(u32, Vec<u8>)> {
    match rx.pump(conn) {
        RxStep::Idle => None,
        RxStep::Frame { msg_type, payload_len, .. } => Some((msg_type, rx.payload(payload_len).to_vec())),
        RxStep::Eof => {
            say!("{at}: soundserver hung up the {which} connection; the stream stops with it");
            std::process::exit(0);
        }
        RxStep::Malformed => panic!("{at}: soundserver sent a {which} frame this protocol cannot describe"),
    }
}

fn soundserver_gone(at: &str, why: impl core::fmt::Debug) -> ! {
    say!("{at}: soundserver took no report ({why:?}); the stream stops with it");
    std::process::exit(0);
}

/// The real-time band the manifest grants this row: the stream's interrupt is
/// a period's end, and what it reports soundserver mixes the next one by.
fn rt() {
    let rt: SysCap = Endowments::get()
        .take(toyos_abi::syscall::SYSCAP_LABEL)
        .expect("the manifest declares this program `syscap = [\"rt\"]`");
    rt.enter_rt().expect("an RT capability refused the band it names");
}
