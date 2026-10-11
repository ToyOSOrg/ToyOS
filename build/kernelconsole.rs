//! The kernel's console as the host reads it off the virtio port: every byte
//! from the kernel's first record on, and none before it.
//!
//! A firmware whose UEFI console drives that port writes its own lines and the
//! loader's there first, the last of them cut off where boot services end and
//! the kernel's first record written onto its tail. Every one of them is on the
//! 16550 as well, and a loader line is read there. Where the kernel begins is
//! the whole rule (`toyos_logstream::kernel_opening`), so a firmware that
//! writes nothing on the port reads the same. A terminal is shown that whole,
//! and what is the kernel's in colour ([`Painter`], [`relay`]); the bytes the
//! host reads are never coloured.

use std::borrow::Cow;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};

use toyos_logstream::{kernel_opening, Opening, Showing};

/// A console's bytes as they arrive, withheld until the kernel's first record.
///
/// Of a QEMU process's first boot only: a guest reset does not re-arm it
/// (`issues/the-kernel-console-split-does-not-re-arm-across-a-guest-reset.md`).
pub struct KernelConsole {
    /// The withheld tail that could still open the kernel's first record;
    /// `None` once the kernel has begun.
    held: Option<Vec<u8>>,
}

impl Default for KernelConsole {
    fn default() -> Self {
        Self { held: Some(Vec::new()) }
    }
}

impl KernelConsole {
    /// What of the next `chunk` is the kernel's.
    pub fn pass<'a>(&mut self, chunk: &'a [u8]) -> Cow<'a, [u8]> {
        let Some(held) = &mut self.held else { return Cow::Borrowed(chunk) };
        held.extend_from_slice(chunk);
        match kernel_opening(held) {
            Opening::At(at) => {
                let from = held.split_off(at);
                self.held = None;
                Cow::Owned(from)
            }
            Opening::From(at) => {
                held.drain(..at);
                Cow::Borrowed(&[])
            }
            Opening::Nowhere => {
                held.clear();
                Cow::Borrowed(&[])
            }
        }
    }
}

/// The console as a terminal shows it: what comes before the kernel's first
/// record as it came, and every line from it on as [`Showing`] shows it.
///
/// From the kernel's first record on the console carries whole lines only, so
/// a line is held until it ends; before it, only what could still open that
/// record is.
#[derive(Default)]
pub struct Painter {
    begun: bool,
    held: Vec<u8>,
    showing: Showing,
}

impl Painter {
    /// What of the next `chunk` the terminal is shown now.
    pub fn pass(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        self.held.extend_from_slice(chunk);
        if !self.begun {
            match kernel_opening(&self.held) {
                Opening::At(at) => {
                    out.extend(self.held.drain(..at));
                    self.begun = true;
                }
                Opening::From(at) => {
                    out.extend(self.held.drain(..at));
                    return out;
                }
                Opening::Nowhere => {
                    out.append(&mut self.held);
                    return out;
                }
            }
        }
        while let Some(end) = self.held.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.held.drain(..=end).collect();
            self.show(&line[..end], &mut out);
            out.push(b'\n');
        }
        out
    }

    /// What the terminal is shown of what is still held once the console has
    /// ended: a line a machine that stopped mid-line never finished, or the
    /// bytes that could have opened the kernel's first record.
    pub fn finish(mut self) -> Vec<u8> {
        let held = std::mem::take(&mut self.held);
        if !self.begun || held.is_empty() {
            return held;
        }
        let mut out = Vec::new();
        self.show(&held, &mut out);
        out
    }

    fn show(&mut self, line: &[u8], out: &mut Vec<u8>) {
        let line = String::from_utf8_lossy(line);
        let line = line.strip_suffix('\r').unwrap_or(&line);
        out.extend_from_slice(self.showing.line(line).to_string().as_bytes());
    }
}

/// Relay `console` to `terminal` as a [`Painter`] shows it, until `console`
/// ends and what the painter held is written.
///
/// **A write the terminal refuses as `WouldBlock` waits for it and goes on.**
/// QEMU's stdio chardev makes its fd 0 non-blocking (`stdio_chr_open`,
/// `chardev/char-stdio.c`), and a terminal's fd 0 and this process's stdout
/// are one open file description, which is what holds the flag.
pub fn relay(mut console: impl Read, terminal: &mut (impl Write + AsFd)) -> io::Result<()> {
    let mut painter = Painter::default();
    let mut buf = [0u8; 4096];
    loop {
        let n = match console.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if n == 0 {
            return write_all(terminal, &painter.finish());
        }
        write_all(terminal, &painter.pass(&buf[..n]))?;
    }
}

fn write_all(out: &mut (impl Write + AsFd), mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        match out.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => writable(out.as_fd())?,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Wait until `fd` takes a write. Unbounded, as a blocking write is: a
/// terminal its user has stopped takes one when the user lets it.
///
/// Any answer but `POLLOUT` is an error, never a retry: a `POLLNVAL` or
/// `POLLERR` comes back at once, and retrying it would spin.
fn writable(fd: BorrowedFd<'_>) -> io::Result<()> {
    let mut ready = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLOUT, revents: 0 };
    // SAFETY: one `pollfd`, which lives across the call.
    if unsafe { libc::poll(&mut ready, 1, -1) } < 0 {
        let e = io::Error::last_os_error();
        return if e.kind() == io::ErrorKind::Interrupted { Ok(()) } else { Err(e) };
    }
    if ready.revents != libc::POLLOUT {
        return Err(io::Error::other(format!("poll answered {:#x} for a terminal, not POLLOUT", ready.revents)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::FromRawFd;

    /// QEMU 11.1.1's own edk2 on the virtio port, as a boot put it there: the
    /// screen clears, `BdsDxe` and the loader, and the loader's last line cut
    /// off by the handoff.
    const FIRMWARE: &str = "\x1b[2J\x1b[01;01H\x1b[=3h\x1b[2J\x1b[01;01HBdsDxe: loading Boot0001 \
         \"UEFI QEMU QEMU USB HARDDRIVE TOYOS0BOOTSTICK1\" from PciRoot(0x0)/Pci(0x1,0x0)/USB(0x0,0x0)\n\
         [--.--- cpu0 loader] ToyOS Bootloader 1.0\n\
         [--.--- cpu0 loader] ROOT: read into memory at 0x7c894000+0x800000 from LBA 212992+16384, 1048576 bytes a request \
         (optimal granularity: not reported), in 22519000 counter ticks\n\
         [--.--- cpu0 loader] Loader log: the kernel handoff begins, so ";

    const KERNEL: &str = "[--.--- cpu0 kernel] black box: 0x8000000 is this boot's, 16344 bytes \
         for the next boot's loader\n\
         [--.--- cpu0 kernel] pmm: the firmware map calls 4288393216 bytes usable\n";

    /// Everything `stream` passes, fed in pieces cut at each of `cuts`.
    fn passed(stream: &str, cuts: &[usize]) -> String {
        let mut console = KernelConsole::default();
        let mut out = Vec::new();
        let mut from = 0;
        for &to in cuts.iter().chain([stream.len()].iter()) {
            out.extend_from_slice(&console.pass(&stream.as_bytes()[from..to]));
            from = to;
        }
        String::from_utf8(out).expect("the kernel's bytes are UTF-8")
    }

    #[test]
    fn the_fused_line_is_the_kernels_first_record_wherever_the_chunks_fall() {
        let stream = format!("{FIRMWARE}{KERNEL}");
        for cut in 0..=stream.len() {
            assert_eq!(passed(&stream, &[cut]), KERNEL, "cut at byte {cut}");
        }
        let every_byte: Vec<usize> = (1..stream.len()).collect();
        let kernel = passed(&stream, &every_byte);
        let first = kernel.lines().next().expect("a first line");
        assert_eq!(
            crate::bootlog::message(first),
            Some("black box: 0x8000000 is this boot's, 16344 bytes for the next boot's loader")
        );
    }

    #[test]
    fn a_port_the_firmware_left_alone_passes_whole() {
        let later = format!("{KERNEL}[ 1.002 supervisor] supervisor: a program's line\n");
        assert_eq!(passed(&later, &[3, 40]), later);
    }

    /// What the terminal is shown of `stream` fed in pieces cut at each of `cuts`.
    fn painted(stream: &str, cuts: &[usize]) -> String {
        let mut painter = Painter::default();
        let mut out = Vec::new();
        let mut from = 0;
        for &to in cuts.iter().chain([stream.len()].iter()) {
            out.extend_from_slice(&painter.pass(&stream.as_bytes()[from..to]));
            from = to;
        }
        String::from_utf8(out).expect("the painter writes UTF-8")
    }

    /// The firmware's bytes reach the terminal as they came, the kernel's
    /// lines as a screen shows them, a continuation in its record's severity —
    /// and the same however the chunks fall.
    #[test]
    fn a_terminal_is_shown_the_firmware_as_it_came_and_the_kernel_in_colour() {
        let alert = "[ 0.002 cpu1 kernel alert tid=4] PANIC: panicked at kernel/src/main.rs:1:\n  oops\n";
        let program = "[ 0.003 supervisor warn] supervisor: a program's line\n";
        let stream = format!("{FIRMWARE}{KERNEL}{alert}{program}");
        let mut want = String::from(FIRMWARE);
        let mut showing = Showing::default();
        for line in format!("{KERNEL}{alert}{program}").lines() {
            want.push_str(&format!("{}\n", showing.line(line)));
        }
        assert!(want.contains("\x1b[91m  oops\x1b[0m\n"), "{want:?}");
        assert_eq!(painted(&stream, &[]), want);
        let every_byte: Vec<usize> = (1..stream.len()).collect();
        assert_eq!(painted(&stream, &every_byte), want);
        // What is held before the kernel is only what could still open its first record.
        assert_eq!(painted(&FIRMWARE[..FIRMWARE.len() - 3], &[]), FIRMWARE[..FIRMWARE.len() - 3]);
        assert_eq!(painted("so [--.--- cpu0 ker", &[]), "so ");
    }

    /// A console that ends mid-line — a machine that stopped while it spoke —
    /// still shows its last line, and one that ends on what could have begun
    /// the kernel's head still shows those bytes.
    #[test]
    fn a_console_that_ends_mid_line_shows_its_last_line() {
        let cut = "[ 0.004 cpu0 kernel alert tid=1] PANIC: triple fa";
        let mut painter = Painter::default();
        let mut out = painter.pass(format!("{KERNEL}{cut}").as_bytes());
        out.extend(painter.finish());
        let mut showing = Showing::default();
        let want: String = KERNEL.lines().map(|line| format!("{}\n", showing.line(line))).collect();
        let want = format!("{want}{}", showing.line(cut));
        assert!(want.ends_with("\x1b[91mPANIC: triple fa\x1b[0m"), "{want:?}");
        assert_eq!(String::from_utf8(out).expect("UTF-8"), want);

        for early in ["so [--.--- cpu0 ker", "loading\r"] {
            let mut painter = Painter::default();
            let mut out = painter.pass(early.as_bytes());
            out.extend(painter.finish());
            assert_eq!(out, early.as_bytes());
        }
    }

    /// A descriptor that refuses a write as `WouldBlock` once it is full, and says so
    /// the first time it does.
    struct Refusing {
        slave: std::fs::File,
        refused: Option<std::sync::mpsc::Sender<()>>,
    }

    impl Write for Refusing {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let wrote = self.slave.write(bytes);
            if wrote.as_ref().is_err_and(|e| e.kind() == io::ErrorKind::WouldBlock) {
                self.refused.take().map(|said| said.send(()));
            }
            wrote
        }

        fn flush(&mut self) -> io::Result<()> {
            self.slave.flush()
        }
    }

    impl AsFd for Refusing {
        fn as_fd(&self) -> BorrowedFd<'_> {
            self.slave.as_fd()
        }
    }

    /// A burst of kernel lines far past a terminal's capacity, and what a
    /// [`Painter`] shows of it.
    fn burst() -> (Vec<u8>, Vec<u8>) {
        let mut stream = String::from(FIRMWARE);
        for n in 0..20_000 {
            stream.push_str(&format!("[ 1.{:03} cpu{} kernel alert tid=3] frame {n}: kernel::panic\n", n % 1000, n % 8));
            stream.push_str("  continued\n");
        }
        stream.push_str("[ 9.999 cpu0 kernel] cut mid-li");
        let mut painter = Painter::default();
        let mut want = painter.pass(stream.as_bytes());
        want.extend(painter.finish());
        (stream.into_bytes(), want)
    }

    fn non_blocking(fd: BorrowedFd<'_>) {
        // SAFETY: `fd` is open for the call.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        // SAFETY: as above; the flags are its own and `O_NONBLOCK`.
        assert!(flags >= 0 && unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0);
    }

    /// **A terminal that refuses a write as `WouldBlock` is waited for, and
    /// shown every byte in order**: a burst far past its capacity into a pty
    /// whose raw, non-blocking slave nobody reads until it has refused a
    /// write. `writable` errs on any `poll` answer but `POLLOUT`, so the
    /// relay's success is `poll` waiting on the device.
    #[test]
    fn a_relay_waits_out_a_pty_that_would_block() {
        let (console, want) = burst();
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: two out-pointers to live ints; no name, termios or size asked for.
        let opened = unsafe {
            libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut())
        };
        assert_eq!(opened, 0, "openpty: {}", io::Error::last_os_error());
        // SAFETY: `openpty` returned both descriptors open and ours alone.
        let (mut master, slave) =
            unsafe { (std::fs::File::from_raw_fd(master), std::fs::File::from_raw_fd(slave)) };
        // SAFETY: a zeroed termios is only a buffer `tcgetattr` fills.
        let mut raw: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `slave` is an open terminal and `raw` lives across both calls.
        assert!(unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut raw) } == 0);
        // SAFETY: as above.
        unsafe { libc::cfmakeraw(&mut raw) };
        // SAFETY: as above.
        assert!(unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &raw) } == 0);
        non_blocking(slave.as_fd());
        let (said, refused) = std::sync::mpsc::channel();
        let mut terminal = Refusing { slave, refused: Some(said) };
        // The slave stays open until the master has read it all: the last close
        // of a non-blocking slave discards what it still queues.
        let (relayed, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || relayed.send((relay(console.as_slice(), &mut terminal), terminal)));
        let length = want.len();
        let (read, all_read) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            refused.recv_timeout(std::time::Duration::from_secs(60)).expect("the pty never refused a write");
            let mut shown = vec![0; length];
            read.send(master.read_exact(&mut shown).map(|()| shown))
        });

        // A relay whose wait never ends reds here rather than hanging the suite.
        let (wrote, _slave) = done
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap_or_else(|e| panic!("the relay did not finish: {e}"));
        wrote.expect("the relay wrote everything");
        // A relay that lost bytes leaves the reader blocked on an open slave.
        let shown = all_read
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap_or_else(|e| panic!("the terminal was not shown the {length} bytes painted: {e}"))
            .expect("the relay's output");
        assert!(shown == want, "the terminal was shown other bytes than the {length} painted");
    }

    #[test]
    fn nothing_before_the_kernel_passes() {
        assert_eq!(passed(FIRMWARE, &[5, 200]), "");
        let mut console = KernelConsole::default();
        assert!(console.pass(FIRMWARE.as_bytes()).is_empty());
        let held = console.held.as_ref().expect("the kernel has not begun").len();
        assert!(held < toyos_logstream::MAX_HEAD, "{held} bytes withheld, more than could open a record");
    }
}
