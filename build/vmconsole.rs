//! The VM's console: at the launch, a virtio-serial function of its own whose
//! one port is named [`CONSOLE_PORT`] and carries what a chardev is given
//! ([`args`]); beside it the UART, whose lines the port cannot carry are
//! merged in ([`merge`], for the launch and the test harness alike); and on
//! the host's terminal, every line in colour ([`relay`]). The bytes the host
//! reads are never coloured.
//!
//! **A `virtserialport`, never a `virtconsole`**: QEMU 11.1.1 holds a
//! `virtserialport`'s transmit until its chardev takes it, measured — a host
//! reader stopped for 20 s lost no byte — where a `virtconsole` dropped what
//! a slow reader left. The port is [`PORT_ID`] of a multiport function.
//! Firmware writes nothing on it, measured, so its first byte is the boot
//! log's.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::Child;
use std::time::{Duration, Instant};

use toyos_logstream::death::{died, Died};
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

/// What the kernel puts on the UART as the console's carrier takes the log
/// (`kernel/src/log/console.rs`): every UART line before it the console port
/// carries too, and the ones after it are the UART's alone.
pub const CARRIED_ELSEWHERE: &str = "console: the log is carried elsewhere from here";

/// Which of a virtio machine's UART lines are shown beside its console port:
/// the loader, the early boot, and the lines the port cannot carry — the
/// stop's, a fatal path's, and the whole log again once the console's carrier
/// is gone or has stalled.
///
/// **Its lines up to [`CARRIED_ELSEWHERE`] are the console port's too**, so
/// they are not shown, but for a kernel's death, which no other channel will
/// carry; every line after either is.
#[derive(Default)]
pub struct Uart {
    pending: Vec<u8>,
    live: bool,
}

impl Uart {
    /// The whole lines of `bytes` to show, each with its line ending.
    pub fn pass(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.pending.extend_from_slice(bytes);
        let mut shown = Vec::new();
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let text = String::from_utf8_lossy(&line);
            if !self.live && text.contains(CARRIED_ELSEWHERE) {
                self.live = true;
                continue;
            }
            self.live |= died(text.trim_end()) == Some(Died::Kernel);
            if self.live {
                shown.push(line);
            }
        }
        shown
    }
}

/// How long QEMU may take to connect to the UART's socket: it does as it makes
/// its chardevs, before the machine runs.
const UART_CONNECT: Duration = Duration::from_secs(30);

/// A virtio machine's UART on a socket the host listens on, and the file every
/// byte it says is kept in.
pub struct UartSocket {
    stream: UnixStream,
    file: File,
}

impl UartSocket {
    /// QEMU's connection on `listener`, waited for while `child` runs and for
    /// [`UART_CONNECT`]; `file` is made to keep what it says.
    pub fn accept(listener: &UnixListener, child: &mut Child, file: &Path) -> Result<Self, String> {
        listener.set_nonblocking(true).map_err(|e| format!("the UART's socket: {e}"))?;
        let deadline = Instant::now() + UART_CONNECT;
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(format!("the UART's socket: {e}")),
            }
            if let Ok(Some(status)) = child.try_wait() {
                return Err(format!("QEMU ended ({status}) before it connected to the UART's socket"));
            }
            if Instant::now() > deadline {
                return Err(format!("QEMU did not connect to the UART's socket in {} s", UART_CONNECT.as_secs()));
            }
            readable(listener.as_raw_fd(), 100);
        };
        stream.set_nonblocking(false).map_err(|e| format!("the UART's socket: {e}"))?;
        let file = File::create(file).map_err(|e| format!("{}: {e}", file.display()))?;
        Ok(Self { stream, file })
    }

    /// The socket, for what the host types at the UART.
    pub fn stream(&self) -> &UnixStream {
        &self.stream
    }
}

/// Whether `fd` reads within `ms`, or at its end.
fn readable(fd: std::os::fd::RawFd, ms: i32) -> bool {
    let mut ready = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    // SAFETY: one `pollfd`, which lives across the call.
    unsafe { libc::poll(&mut ready, 1, ms) > 0 }
}

/// Hand `show` every line of the console `port` and, on a virtio machine, the
/// [`Uart`] lines of `uart`, each with its line ending, until the port ends,
/// and then the port's last line if it never ended. `show` answers `false`
/// once nothing is left to read for.
///
/// **One thread reads both, and drains the port before each read of the
/// UART**: QEMU writes a console port's bytes out before the guest is told the
/// host took them, so what the guest said there before it said a line on the
/// UART is readable by the time that line is, and is shown first.
pub fn merge(mut port: impl Read + AsRawFd, mut uart: Option<UartSocket>, mut show: impl FnMut(&[u8]) -> bool) {
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut filter = Uart::default();
    // One read of the port.
    let mut out = |port: &mut dyn Read, show: &mut dyn FnMut(&[u8]) -> bool| -> PortRead {
        let read = loop {
            match port.read(&mut chunk) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                got => break got.unwrap_or(0),
            }
        };
        if read == 0 {
            if !pending.is_empty() && !show(&std::mem::take(&mut pending)) {
                return PortRead::Stop;
            }
            return PortRead::Ended;
        }
        pending.extend_from_slice(&chunk[..read]);
        while let Some(end) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            if !show(&line) {
                return PortRead::Stop;
            }
        }
        PortRead::More
    };
    loop {
        let Some(console) = &mut uart else {
            if out(&mut port, &mut show) != PortRead::More {
                return;
            }
            continue;
        };
        let mut fds = [
            libc::pollfd { fd: port.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: console.stream.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        // SAFETY: two `pollfd`s, which live across the call.
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
            continue;
        }
        let mut port_read = fds[0].revents != 0;
        let uart_read = fds[1].revents != 0;
        while port_read {
            match out(&mut port, &mut show) {
                PortRead::More => port_read = readable(port.as_raw_fd(), 0),
                // The UART may still hold what it said last, the stop's
                // last lines or a fatal report's tail: read to its end too.
                PortRead::Ended => return console.drain(&mut filter, &mut show),
                PortRead::Stop => return,
            }
        }
        if uart_read && !console.read_once(&mut filter, &mut show) {
            uart = None;
        }
    }
}

/// What one read of the console port came to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PortRead {
    More,
    Ended,
    /// `show` asked for nothing more.
    Stop,
}

impl UartSocket {
    /// One read: its bytes into the file and its lines to show through
    /// `filter`. `false` at the socket's end, or once `show` asks for nothing
    /// more.
    fn read_once(&mut self, filter: &mut Uart, show: &mut impl FnMut(&[u8]) -> bool) -> bool {
        let mut bytes = [0u8; 4096];
        let n = match self.stream.read(&mut bytes) {
            Ok(0) | Err(_) => return false,
            Ok(n) => n,
        };
        // The file keeps every byte; a host that cannot write it has lost the
        // record, and says so by ending.
        if let Err(e) = self.file.write_all(&bytes[..n]) {
            panic!("the UART's file: {e}");
        }
        filter.pass(&bytes[..n]).iter().all(|line| show(line))
    }

    /// Every read to the socket's end, which comes with QEMU's own.
    fn drain(&mut self, filter: &mut Uart, show: &mut impl FnMut(&[u8]) -> bool) {
        while self.read_once(filter, show) {}
    }
}

/// Show `terminal` the console [`merge`] reads off `port` and `uart`, as a
/// [`Painter`] shows it, until the port ends and what the painter held is
/// written.
///
/// **A write the terminal refuses as `WouldBlock` waits for it and goes on.**
/// QEMU's stdio chardev makes its fd 0 non-blocking (`stdio_chr_open`,
/// `chardev/char-stdio.c`), and a terminal's fd 0 and this process's stdout
/// are one open file description, which is what holds the flag.
pub fn relay(port: impl Read + AsRawFd, uart: Option<UartSocket>, terminal: &mut (impl Write + AsFd)) -> io::Result<()> {
    let mut painter = Painter::default();
    let mut failed = None;
    merge(port, uart, |line| match write_all(terminal, &painter.pass(line)) {
        Ok(()) => true,
        Err(e) => {
            failed = Some(e);
            false
        }
    });
    match failed {
        Some(e) => Err(e),
        None => write_all(terminal, &painter.finish()),
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
        let (port, mut feeder) = UnixStream::pair().expect("a socket pair for the console port");
        std::thread::spawn(move || feeder.write_all(&console).expect("the port's bytes"));
        std::thread::spawn(move || relayed.send((relay(port, None, &mut terminal), terminal)));
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

    const QUIET: &str = "[ 6.459 cpu0 kernel] console: the log is carried elsewhere from here; the UART takes the stop's and a fatal path's lines\n";

    /// The UART's lines before the quiet line are the port's too and are kept
    /// back; the quiet line itself says nothing the operator needs, and every
    /// line after it is shown, however the bytes are cut.
    #[test]
    fn the_uart_is_shown_from_the_carriers_word_on() {
        let early = "[ 0.001 cpu0 kernel] boot: early\n[ 4.000 supervisor] supervisor: started logkeeper\n";
        let late = "[ 9.000 cpu0 kernel] console: the UART carries the log again\n[ 9.100 supervisor] supervisor: x ended\n";
        let stream = format!("{early}{QUIET}{late}");
        for cut in [1, 7, stream.len()] {
            let mut uart = Uart::default();
            let shown: Vec<u8> = stream.as_bytes().chunks(cut).flat_map(|c| uart.pass(c)).flatten().collect();
            assert_eq!(String::from_utf8(shown).expect("UTF-8"), late, "cut every {cut} bytes");
        }
    }

    /// A kernel that dies before any carrier took the log has its report on
    /// the UART alone, so it is shown from its first line; a program's own
    /// panic before the quiet line is the port's like any program line.
    #[test]
    fn a_kernels_death_before_the_carrier_is_shown() {
        let program = "[ 3.000 supervisor] PANIC: printed by the supervisor\n";
        let death = "[ 3.100 cpu1 kernel alert] PANIC: panicked at kernel/src/main.rs:1:\n  oops\n";
        let mut uart = Uart::default();
        let shown: Vec<u8> = uart.pass(format!("{program}{death}").as_bytes()).concat();
        assert_eq!(String::from_utf8(shown).expect("UTF-8"), death);
    }

    /// **The launch's merge**: the port's lines in order, then the UART's from
    /// the carrier's word on, the port drained before each UART read; the
    /// port's last unended line at its end; and what the UART says after the
    /// port has ended, shown and in its file.
    #[test]
    fn the_merge_shows_the_port_and_the_uarts_own_lines_in_order() {
        let (port, mut port_in) = UnixStream::pair().expect("a pair for the port");
        let (uart, mut uart_in) = UnixStream::pair().expect("a pair for the UART");
        let dir = toyos_tmpdir::TempDir::new("vmconsole-merge");
        let path = dir.path().join("uart.log");
        let file = File::create(&path).expect("the UART's file");
        let port_text = "[ 0.001 cpu0 kernel] boot: early\n[ 6.500 supervisor] supervisor: started x\n";
        let death = "[ 7.000 cpu0 kernel alert] PANIC: panicked at a:1:\n";
        let last = "[ 7.002 cpu0 kernel] Shutting down.\n";
        let uart_text = format!("[ 0.001 cpu0 kernel] boot: early\n{QUIET}{death}");
        port_in.write_all(port_text.as_bytes()).expect("the port's bytes");
        uart_in.write_all(uart_text.as_bytes()).expect("the UART's bytes");
        // Once the UART's line is shown: the port's last, unended bytes and
        // its end, and only then the UART's last line and its end, as QEMU's
        // exit can leave them.
        let (seen, seen_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let ender = std::thread::spawn(move || {
            while seen_rx.recv().is_ok_and(|line| line != death.as_bytes()) {}
            port_in.write_all(b"[ 7.001 cpu0 kernel] cut mid-li").expect("the port's last bytes");
            drop(port_in);
            while seen_rx.recv().is_ok_and(|line| !line.ends_with(b"cut mid-li")) {}
            uart_in.write_all(last.as_bytes()).expect("the UART's last line");
        });
        let mut shown = Vec::new();
        merge(port, Some(UartSocket { stream: uart, file }), |line| {
            shown.extend_from_slice(line);
            let _ = seen.send(line.to_vec());
            true
        });
        ender.join().expect("the writer");
        let want = format!("{port_text}{death}[ 7.001 cpu0 kernel] cut mid-li{last}");
        assert_eq!(String::from_utf8(shown).expect("UTF-8"), want);
        assert_eq!(std::fs::read_to_string(&path).expect("the UART's file"), format!("{uart_text}{last}"));
    }

}
