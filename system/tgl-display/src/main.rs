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
            serve(&dev, &regs, &done);
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

/// The firmware's output offered on `display-output`, once: when the
/// connection ends, for whatever reason is said, presents stop and the
/// scan-out stays. A connection the compositor refused would only be refused
/// again.
fn serve(dev: &PciDev, regs: &Bar, done: &Programmed) {
    let size = Size { width: done.scanout.width as u32, height: done.scanout.height as u32 };
    interrupt::claim(regs);
    let conn = match endow::service(PORT) {
        Ok(conn) => conn,
        Err(why) => return say!("tgl-display: no `{PORT}` connection ({why:?}); presents go unanswered"),
    };
    if let Err(why) = conn.send_bytes(MSG_HELLO, &Hello { size }.encode()) {
        return say!("tgl-display: the compositor took no hello ({why:?}); presents go unanswered");
    }
    say!("tgl-display: offered the compositor its {}x{} output", size.width, size.height);
    let why = answer(dev, regs, &conn, size, done.refresh_interval_nanos());
    say!("tgl-display: presents go unanswered — {why}");
}

/// Every present on `conn` answered at the vblank after it arrived, until
/// one cannot be: why.
fn answer(dev: &PciDev, regs: &Bar, conn: &Connection, size: Size, refresh_interval_nanos: u64) -> String {
    let poller = Poller::new(2);
    let mut rx = FrameRx::<MAX_FRAME>::new();
    // The present waiting for its vblank, and when that vblank is overdue.
    let mut outstanding: Option<(u64, u64)> = None;
    loop {
        poller.watch(dev, READABLE, TOKEN_CLAIM);
        poller.watch(conn, READABLE, TOKEN_COMPOSITOR);
        let timeout = outstanding.map_or(u64::MAX, |(_, overdue)| overdue.saturating_sub(toyos_abi::clock::nanos_since_boot()));
        poller.wait(1, timeout, |_| {});

        match dev.irq() {
            Ok(record) => match interrupt::serve(regs) {
                Ok(true) => {
                    if let Some((sequence, _)) = outstanding.take() {
                        let presented = Presented { sequence, vblank_nanos: record.last_nanos, refresh_interval_nanos };
                        if let Err(why) = conn.send_bytes(MSG_PRESENTED, &presented.encode()) {
                            return format!("the compositor took no answer ({why:?})");
                        }
                    }
                }
                Ok(false) => {}
                Err(why) => return why.to_string(),
            },
            Err(SyscallError::WouldBlock) => {}
            Err(why) => panic!("tgl-display: the claim's interrupt record: {why:?}"),
        }
        if outstanding.is_some_and(|(_, overdue)| toyos_abi::clock::nanos_since_boot() >= overdue) {
            return format!("no vblank within {} ms of a present", FRAME_CEILING_NANOS / 1_000_000);
        }

        loop {
            let (msg_type, payload_len) = match rx.pump(conn) {
                RxStep::Idle => break,
                RxStep::Frame { msg_type, payload_len, .. } => (msg_type, payload_len),
                RxStep::Eof => return format!("the compositor hung up `{PORT}`"),
                RxStep::Malformed => return "the compositor sent a frame no protocol here can make".into(),
            };
            let present = Present::decode(msg_type, rx.payload(payload_len), size);
            match (present, outstanding) {
                (Some(present), None) => {
                    interrupt::arm(regs);
                    outstanding = Some((present.sequence, toyos_abi::clock::nanos_since_boot() + FRAME_CEILING_NANOS));
                }
                (Some(present), Some((waiting, _))) => {
                    return format!("the compositor sent present {} while {waiting} waits", present.sequence);
                }
                (None, _) => return format!("the compositor sent frame {msg_type}, which `{PORT}` has not"),
            }
        }
    }
}
