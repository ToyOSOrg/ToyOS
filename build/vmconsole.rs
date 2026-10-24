//! The VM's console: at the launch, a virtio-serial function of its own whose
//! one port is named [`CONSOLE_PORT`] and carries what a chardev is given
//! ([`args`]); and on the host's terminal, every line in colour ([`relay`]).
//! The bytes the host reads are never coloured.
//!
//! **A `virtserialport`, never a `virtconsole`**: QEMU 11.1.1 holds a
//! `virtserialport`'s transmit until its chardev takes it, and drops what a
//! `virtconsole`'s chardev refuses (`hw/char/virtio-console.c`, `flush_buf`).
//! Port 0 is a `virtconsole`'s alone, so the port is [`PORT_ID`] of a
//! multiport function. Firmware writes nothing on it, so its first byte is
//! the boot log's.

use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};

use toyos_logstream::Showing;
use toyos_virtio_serial::{CONSOLE_PORT, MAX_PORTS, PORT_ID};

/// The port's QEMU device id, which a test unplugs it by.
pub const PORT_DEVICE: &str = "console-port";

/// The arguments that put the console's port on `chardev`. `platform` is the
/// virtio function's `iommu_platform` property, as every other virtio function
/// on the machine is given it.
pub fn args(chardev: &str, platform: &str) -> [String; 4] {
    [
        "-device".into(),
        format!("virtio-serial-pci-non-transitional,id=console-serial,max_ports={MAX_PORTS}{platform}"),
        "-device".into(),
        format!("virtserialport,bus=console-serial.0,nr={PORT_ID},chardev={chardev},name={CONSOLE_PORT},id={PORT_DEVICE}"),
    ]
}

/// The console as a terminal shows it: every line as [`Showing`] shows it,
/// each held until it ends.
#[derive(Default)]
pub struct Painter {
    held: Vec<u8>,
    showing: Showing,
}

impl Painter {
    /// What of the next `chunk` the terminal is shown now.
    pub fn pass(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        self.held.extend_from_slice(chunk);
        while let Some(end) = self.held.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.held.drain(..=end).collect();
            self.show(&line[..end], &mut out);
            out.push(b'\n');
        }
        out
    }

    /// What the terminal is shown of a line a machine that stopped mid-line
    /// never finished, once the console has ended.
    pub fn finish(mut self) -> Vec<u8> {
        let held = std::mem::take(&mut self.held);
        let mut out = Vec::new();
        if !held.is_empty() {
            self.show(&held, &mut out);
        }
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

    const KERNEL: &str = "[--.--- cpu0 kernel] black box: 0x8000000 is this boot's, 16344 bytes \
         for the next boot's loader\n\
         [--.--- cpu0 kernel] pmm: the firmware map calls 4288393216 bytes usable\n";

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

    /// Every line as a screen shows it, a continuation in its record's
    /// severity, and the same however the chunks fall.
    #[test]
    fn a_terminal_is_shown_each_line_in_colour() {
        let alert = "[ 0.002 cpu1 kernel alert tid=4] PANIC: panicked at kernel/src/main.rs:1:\n  oops\n";
        let program = "[ 0.003 supervisor warn] supervisor: a program's line\n";
        let stream = format!("{KERNEL}{alert}{program}");
        let mut want = String::new();
        let mut showing = Showing::default();
        for line in stream.lines() {
            want.push_str(&format!("{}\n", showing.line(line)));
        }
        assert!(want.contains("\x1b[91m  oops\x1b[0m\n"), "{want:?}");
        assert_eq!(painted(&stream, &[]), want);
        let every_byte: Vec<usize> = (1..stream.len()).collect();
        assert_eq!(painted(&stream, &every_byte), want);
    }

    /// A console that ends mid-line — a machine that stopped while it spoke —
    /// still shows its last line.
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
        let mut stream = String::new();
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
}
