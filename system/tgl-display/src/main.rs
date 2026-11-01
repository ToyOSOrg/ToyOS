//! The Tiger Lake display driver: one process for the iGPU, holding its claim
//! (a `drives` row) for the life of the boot.
//!
//! **Every decision is `toyos-tgl-display`'s**; this file is the claim, its
//! register window, the clock and the compositor's `display-output`
//! connection. The firmware's mode and surface stay the GOP's, which the
//! compositor draws on through the kernel's framebuffer claim; the claim's
//! domain keeps the firmware's reserved memory mapped, so the scan-out reaches
//! the framebuffer across the hand-over. Each present the compositor sends is
//! answered at the vblank after it.
//!
//! **The process never exits**: the claim's release resets the function,
//! which ends the scan-out it was given.

use toyos::ipc::{FrameRx, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::{endow, say, Connection, PciDev};
use toyos_abi::syscall::{PciId, RegWidth, SyscallError};
use toyos_display_output::{Hello, Present, Presented, Size, MAX_FRAME, MSG_HELLO, MSG_PRESENTED, PORT};
use toyos_device_memory::Registers;
use toyos_manifest::device::PciMatch;
use toyos_pci_claim::Bar;
use toyos_tgl_display::dmc::{Package, Stepping, TGL_PACKAGE};
use toyos_tgl_display::psr::{self, Prepared, Psr};
use toyos_tgl_display::{interrupt, Programmed, Refusal, FRAME_CEILING_NANOS};

/// The T14's iGPU, Tiger Lake GT2 (`8086:9a49`), a VGA-compatible display
/// controller, class `03:00:00`, as the row spells it.
const FUNCTION: PciMatch = PciMatch::new([0x03, 0x00, 0x00], Some(PciId { vendor: 0x8086, device: 0x9a49 }));

/// GTTMMADR: the registers in its first 2 MiB (Vol 2c GTTMMADR_0_2_0_PCI).
const BAR: u32 = 0;

struct Monotonic;

impl toyos_tgl_display::Clock for Monotonic {
    fn nanos(&self) -> u64 {
        toyos_abi::clock::nanos_since_boot()
    }

    fn pause(&self, nanos: u64) {
        std::thread::sleep(std::time::Duration::from_nanos(nanos));
    }
}

fn main() {
    let dev = endow::claim::<PciDev>(&FUNCTION.to_string()).expect("tgl-display: the drives row endows this process the iGPU's claim");
    let info = toyos_pci_claim::describe(&dev).unwrap_or_else(|why| panic!("tgl-display: {why}"));
    // The window covers only the registers this driver reaches, in whole
    // pages, and not the GGTT above them: the kernel maps the BAR whole, and
    // the window is what bounds every access (`Registers`' contract).
    let span = (toyos_tgl_display::regs::SPAN as u64).next_multiple_of(4096);
    let bytes = info.bar_bytes[BAR as usize];
    assert!(bytes >= span, "tgl-display: BAR {BAR} is {bytes:#x} bytes, short of the registers this driver reaches");
    let (regs, _mapped) = Bar::map(&dev, BAR, span).unwrap_or_else(|why| panic!("tgl-display: {why}"));
    // The firmware's scan-out is a transfer this function masters: started at
    // once, since the claim's domain already maps what it reads.
    toyos_pci_claim::start(&dev).unwrap_or_else(|why| panic!("tgl-display: {why}"));
    say!("tgl-display: PCI {:02x}:{:02x}.{} registers in BAR {BAR}, {span:#x} of its {bytes:#x} bytes", info.bus, info.dev, info.func);

    let package = Package::parse(TGL_PACKAGE).unwrap_or_else(|why| panic!("tgl-display: the DMC package is malformed: {why:?}"));
    let revision = dev.config_read(0x08, RegWidth::U32).unwrap_or_else(|why| panic!("tgl-display: config space: {why:?}")) as u8;

    match toyos_tgl_display::keep_memory_low_power(&regs, &Monotonic) {
        Ok(done) => {
            say!(
                "tgl-display: plane {} {} px wide, totals {}x{}, {} blocks, at {} kHz; latencies {} {:?} us, SAGV block {} us; WM levels were {:08x?}",
                done.scanout.plane,
                done.scanout.width,
                done.scanout.htotal,
                done.scanout.vtotal,
                done.scanout.allocation,
                done.pixel_rate_khz,
                done.latencies.zero,
                done.latencies.above,
                done.sagv_block_us,
                done.before,
            );
            let now: Vec<String> = (0..toyos_tgl_display::regs::LEVELS)
                .map(|level| format!("{:08x}", regs.read32(toyos_tgl_display::regs::plane_wm(0, done.scanout.plane, level))))
                .collect();
            say!("tgl-display: WM levels now {}", now.join(" "));
            // Every AUX transaction comes before DC6 is allowed and PSR enabled.
            let prepared = psr::read_sink(&regs, &Monotonic).and_then(|sink| {
                say!(
                    "tgl-display: panel DPCD 00000h {:02x?}, 00070h {:02x?}, 00700h {:02x?}, 02006h {:02x?}",
                    sink.receiver,
                    sink.psr,
                    sink.edp,
                    sink.status,
                );
                psr::prepare(&regs, &Monotonic, &done, sink)
            });
            match &prepared {
                Ok(p) => say!(
                    "tgl-display: PSR setup {} of {} vblank lines; configured for PSR1 with the main link active",
                    p.setup_lines,
                    p.vblank_lines,
                ),
                Err(why) => say!("tgl-display: PSR stays off — {why}"),
            }
            let allowed = Stepping::of_9a49(revision)
                .ok_or(Refusal::Revision(revision))
                .and_then(|stepping| package.main_program(stepping).ok_or(Refusal::NoProgram(stepping)))
                .and_then(|program| {
                    toyos_tgl_display::allow_display_c6(&regs, &Monotonic, &program).map(|allowed| (program, allowed))
                });
            match allowed {
                Ok((program, allowed)) => say!(
                    "tgl-display: DMC package {:08x}, main program {:08x} at {:#x} for revision {revision:#04x}, DDI select {:#x}; PWR_WELL_CTL1/2 and DC_STATE_EN were {:08x?}, now {:08x} {:08x} {:08x}, AUDIO_PIN_BUF_CTL {:08x}; pipe A frame {}",
                    package.version,
                    program.version,
                    program.start,
                    allowed.ddi,
                    allowed.before,
                    regs.read32(toyos_tgl_display::regs::PWR_WELL_CTL1),
                    regs.read32(toyos_tgl_display::regs::PWR_WELL_CTL2),
                    regs.read32(toyos_tgl_display::regs::DC_STATE_EN),
                    regs.read32(toyos_tgl_display::regs::AUDIO_PIN_BUF_CTL),
                    regs.read32(toyos_tgl_display::regs::PIPE_FRMCNT_A),
                ),
                Err(why) => say!("tgl-display: DC5 and DC6 stay disallowed — {why}"),
            }
            serve(&dev, &regs, &done, prepared.ok());
        }
        Err(why) => say!("tgl-display: the firmware's display is left as found — {why}"),
    }
    park()
}

fn park() -> ! {
    loop {
        std::thread::park();
    }
}

const TOKEN_CLAIM: u64 = 0;
const TOKEN_COMPOSITOR: u64 = 1;

/// How many times the driver connects again after a connection a present
/// arrived on ended with the compositor gone. Past it, presents stop for the
/// boot.
const RECONNECTS: u32 = 8;

/// Why a connection ended.
enum Ended {
    /// The connection ended from the compositor's side: it hung up, or took
    /// no answer. That is the compositor going away, or it dropping this
    /// driver for a fault of the driver's own, which [`RECONNECTS`] bounds.
    /// `presented`: a present arrived on it, which the compositor sends only
    /// once it has taken the hello.
    Gone { presented: bool, why: String },
    /// A frame out of protocol, or this driver could not answer: connecting
    /// again would end the same way.
    Refused(String),
}

/// The firmware's output offered on `display-output`, again after a
/// connection a present arrived on ends from the compositor's side, at most
/// [`RECONNECTS`] times. The connection is made at once whether or not the
/// compositor serves yet; the next present is the event that it does.
/// Presents stop, and say why, at a refusal, or a connection that ended
/// before any present arrived on it; the scan-out stays either way.
///
/// PSR is enabled only while a connection carries presents, from its first,
/// which is the whole output as the compositor stands, and disabled when the
/// connection ends: nothing else would end SRD for a change. The panel stays
/// configured, so the next connection's first present enables it again. A
/// PSR fault disables it for the boot.
fn serve(dev: &PciDev, regs: &Bar, done: &Programmed, mut prepared: Option<Prepared>) {
    let size = Size { width: done.scanout.width as u32, height: done.scanout.height as u32 };
    interrupt::claim(regs);
    let mut reconnects = 0;
    loop {
        let conn = match endow::service(PORT) {
            Ok(conn) => conn,
            Err(why) => break say!("tgl-display: no `{PORT}` connection ({why:?}); presents go unanswered"),
        };
        if let Err(why) = conn.send_bytes(MSG_HELLO, &Hello { size }.encode()) {
            break say!("tgl-display: the compositor took no hello ({why:?}); presents go unanswered");
        }
        say!("tgl-display: offered the compositor its {}x{} output", size.width, size.height);
        let frame = done.refresh_interval_nanos();
        let mut psr = None;
        let ended = answer(dev, regs, &conn, size, frame, &mut prepared, &mut psr);
        if let Some(psr) = psr {
            let (idle, again) = psr.disable(regs, &Monotonic, frame);
            prepared = Some(again);
            match idle {
                true => say!("tgl-display: PSR disabled with the connection"),
                false => say!(
                    "tgl-display: PSR disabled with the connection, and SRD_STATUS {:08x} is not IDLE",
                    regs.read32(toyos_tgl_display::regs::SRD_STATUS_A)
                ),
            }
        }
        match ended {
            Ended::Gone { presented: true, why } if reconnects < RECONNECTS => {
                reconnects += 1;
                say!("tgl-display: {why}; connecting again ({reconnects} of {RECONNECTS})");
            }
            Ended::Gone { presented: true, why } => {
                break say!("tgl-display: presents go unanswered — {why}, after {RECONNECTS} connections again");
            }
            Ended::Gone { presented: false, why } | Ended::Refused(why) => break say!("tgl-display: presents go unanswered — {why}"),
        }
    }
    interrupt::release(regs);
}

/// A present waiting for the vblank that carries it.
struct Waiting {
    sequence: u64,
    arrived_nanos: u64,
    /// SRD_STATUS when it arrived, where PSR is enabled.
    srd: Option<u32>,
    overdue_nanos: u64,
}

/// Presents' latencies from arrival to the vblank that carried them: each of
/// the first [`WINDOW`] said, then windows of it, apart by whether SRD had to
/// end first.
#[derive(Default)]
struct Latencies {
    asleep: Vec<u64>,
    awake: Vec<u64>,
    said: u32,
}

const WINDOW: usize = 16;

impl Latencies {
    fn record(&mut self, regs: &Bar, waiting: &Waiting, vblank_nanos: u64) {
        let latency = vblank_nanos.saturating_sub(waiting.arrived_nanos);
        let asleep = waiting.srd.is_some_and(|srd| !matches!(srd >> toyos_tgl_display::regs::SRD_STATE_SHIFT, 0b000 | 0b001));
        if self.said < WINDOW as u32 {
            self.said += 1;
            say!(
                "tgl-display: present {} took {latency} ns to its vblank; SRD_STATUS {:08x?} at arrival, {:08x} after",
                waiting.sequence,
                waiting.srd,
                regs.read32(toyos_tgl_display::regs::SRD_STATUS_A),
            );
        }
        if asleep { &mut self.asleep } else { &mut self.awake }.push(latency);
        if self.asleep.len() + self.awake.len() == WINDOW {
            let spread = |v: &mut Vec<u64>| {
                v.sort_unstable();
                match (v.first(), v.last()) {
                    (Some(min), Some(max)) => format!("{} min {min} p50 {} max {max} ns", v.len(), v[v.len() / 2]),
                    _ => "none".into(),
                }
            };
            say!(
                "tgl-display: present latency, SRD ending first: {}; awake: {}; {} ms in SRD so far",
                spread(&mut self.asleep),
                spread(&mut self.awake),
                regs.read32(toyos_tgl_display::regs::SRD_PERF_CNT_A) & 0xFF_FFFF,
            );
            self.asleep.clear();
            self.awake.clear();
        }
    }
}

/// Every present on `conn` answered at the first vblank after it arrived at
/// which the source is out of SRD, until one cannot be: why. PSR is enabled
/// out of `prepared` at the first present, into `psr`.
fn answer(
    dev: &PciDev,
    regs: &Bar,
    conn: &Connection,
    size: Size,
    refresh_interval_nanos: u64,
    prepared: &mut Option<Prepared>,
    psr: &mut Option<Psr>,
) -> Ended {
    let poller = Poller::new(2);
    let mut rx = FrameRx::<MAX_FRAME>::new();
    let mut waiting: Option<Waiting> = None;
    let mut latencies = Latencies::default();
    let mut answered = false;
    let mut presented = false;
    loop {
        poller.watch(dev, READABLE, TOKEN_CLAIM);
        poller.watch(conn, READABLE, TOKEN_COMPOSITOR);
        let now = toyos_abi::clock::nanos_since_boot();
        poller.wait(1, waiting.as_ref().map_or(u64::MAX, |w| w.overdue_nanos.saturating_sub(now)), |_| {});

        match dev.irq() {
            Ok(record) => match interrupt::serve(regs) {
                Ok(true) => {
                    if let Some(why) = psr.as_ref().and_then(|p| p.fault(regs)) {
                        say!("tgl-display: PSR disabled for the boot — {why}");
                        *psr = None;
                    }
                    match (&*psr, waiting.take()) {
                        (Some(p), Some(w)) if !p.awake(regs) => {
                            interrupt::arm(regs);
                            waiting = Some(w);
                        }
                        (_, Some(w)) => {
                            let answer = Presented { sequence: w.sequence, vblank_nanos: record.last_nanos, refresh_interval_nanos };
                            if let Err(why) = conn.send_bytes(MSG_PRESENTED, &answer.encode()) {
                                return Ended::Gone { presented, why: format!("the compositor took no answer ({why:?})") };
                            }
                            if !answered {
                                answered = true;
                                say!("tgl-display: first present answered at pipe A frame {}", regs.read32(toyos_tgl_display::regs::PIPE_FRMCNT_A));
                            }
                            latencies.record(regs, &w, record.last_nanos);
                        }
                        (_, None) => {}
                    }
                }
                Ok(false) => {}
                Err(why) => return Ended::Refused(why.to_string()),
            },
            Err(SyscallError::WouldBlock) => {}
            Err(why) => panic!("tgl-display: the claim's interrupt record: {why:?}"),
        }
        if waiting.as_ref().is_some_and(|w| toyos_abi::clock::nanos_since_boot() >= w.overdue_nanos) {
            return Ended::Refused(format!("no vblank carried a present within {} ms", FRAME_CEILING_NANOS / 1_000_000));
        }

        loop {
            let (msg_type, payload_len) = match rx.pump(conn) {
                RxStep::Idle => break,
                RxStep::Frame { msg_type, payload_len, handles: 0 } => (msg_type, payload_len),
                RxStep::Frame { msg_type, .. } => {
                    return Ended::Refused(format!("the compositor sent frame {msg_type} with handles, which `{PORT}` has none of"));
                }
                RxStep::Eof => return Ended::Gone { presented, why: format!("the compositor hung up `{PORT}`") },
                RxStep::Malformed => return Ended::Refused("the compositor sent a frame no protocol here can make".into()),
            };
            let present = Present::decode(msg_type, rx.payload(payload_len), size);
            match (present, &waiting) {
                (Some(present), None) => {
                    presented = true;
                    if let Some(p) = prepared.take() {
                        *psr = Some(p.activate(regs));
                        say!("tgl-display: PSR1 enabled at the first present, SRD_CTL {:08x}", regs.read32(toyos_tgl_display::regs::SRD_CTL_A));
                    }
                    let arrived_nanos = toyos_abi::clock::nanos_since_boot();
                    let srd = psr.as_ref().map(|p| p.update(regs));
                    interrupt::arm(regs);
                    waiting = Some(Waiting { sequence: present.sequence, arrived_nanos, srd, overdue_nanos: arrived_nanos + FRAME_CEILING_NANOS });
                }
                (Some(present), Some(w)) => {
                    return Ended::Refused(format!("the compositor sent present {} while {} waits", present.sequence, w.sequence));
                }
                (None, _) => return Ended::Refused(format!("the compositor sent frame {msg_type}, which `{PORT}` has not")),
            }
        }
    }
}
