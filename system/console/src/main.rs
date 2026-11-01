//! The machine's console: `/system/bin/shell` on the raw framebuffer, no compositor.
//!
//! It exists for a machine with no serial port. `--diag-boot` freezes the
//! kernel's boot log on the panel, which answers "how far did it get and what
//! did it say" and nothing else — every further question costs a reflash and a
//! photograph. This program answers questions by being asked them.
//!
//! Three things follow from that and are not incidental:
//!
//! - **It shows this boot's log, and keeps showing it.** Claiming
//!   `DEVICE_FRAMEBUFFER` stops `panic_console::boot_checkpoint` from ever
//!   painting again, so a console that merely cleared the screen would trade
//!   the diagnostic that works today for one that might. It asks `logkeeper` for the
//!   log ([`toyos_logstream::SERVICE`]) and draws the boot so far above the
//!   first prompt — the kernel's records and every program's output — and
//!   every program's line after as it is written, each under its program's
//!   name and above the shell's unfinished line (`Log::draw`), as a terminal
//!   shows a line of the log ([`toyos_logstream::Showing`]).
//! - **A fatal panic still takes the screen back.** `render` ignores
//!   `SCREEN_OWNED_BY_USERLAND` entirely — only boot checkpoints honour it —
//!   so the report paints over whatever this program drew.
//! - **The emulator is `/system/bin/terminal`'s**, unchanged. `Console::new` always
//!   took a raw mapping; the compositor was never below it. This is the caller
//!   whose mapping is the scanout, so it is the one that pays for a read.

use std::io::{Read, Write};
use std::os::toyos::process;
use std::os::toyos::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

use terminal::Console;
use toyos::ipc::{Connection, FrameRx, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::endow;
use toyos::port::{self, Connector};
use toyos::surface::{self, Delivery, Host, Notice};
use toyos::{FramebufferDev, Pipe};
use toyos_abi::input::RawKeyEvent;
use toyos_abi::syscall::{DeviceType, SyscallError};
use toyos_input::wire::{self, Record, CONNECTIONS, DEVICES, MAX_PAYLOAD};
use toyos_input::{Merge, Slot};
use toyos_logstream::{Lines, Showing, Source};
use window::Screen;

const FONT: &str = "/system/share/fonts/JetBrainsMono-Regular-8x16.font";

/// HID usage codes. `toyos_keymap::Translator` turns both into escape
/// sequences; this program consumes them before it asks.
const KEY_PAGE_UP: u8 = 0x4B;
const KEY_PAGE_DOWN: u8 = 0x4E;

/// The most frames one input source's turn takes.
const SOURCE_FRAMES: usize = 64;

/// The most of the boot so far [`seed_tail`] will look at.
///
/// **A bound on this program's work.** The scrollback bound below is what
/// normally decides how far back the seed goes; this one is what stops a log
/// with no newlines in it from being walked end to end before that bound can
/// apply.
const SEED_MAX_BYTES: usize = 64 * 1024;

/// The name the supervisor starts this program under, which its own lines
/// come back from `logkeeper` tagged with.
const OWN_TAG: &str = "console";

/// This boot's log as `logkeeper` hands it to a reader on this machine: a pipe the
/// log is written into, and how many of its bytes are the boot so far.
struct Log {
    pipe: Pipe,
    lines: Lines,
    handed: u64,
    /// When this program asked, in the log's milliseconds: a kernel record
    /// stamped before it is the boot so far, however late `logkeeper` read it.
    asked_ms: u64,
    /// Whether the last kernel record was kept, which its continuation lines
    /// follow.
    drawing: bool,
    /// How each line is shown.
    showing: Showing,
    /// Lines kept and not yet drawn.
    held: Vec<u8>,
}

impl Log {
    /// Ask `logkeeper`: one request, and a blocking read of its one answer.
    fn subscribe() -> Result<Self, String> {
        let asked_ms = toyos_abi::clock::stamp_ns() / 1_000_000;
        let logkeeper_api::Served { pipe, boot_so_far: handed } = logkeeper_api::read()?;
        Ok(Self {
            pipe,
            lines: Lines::new(),
            handed,
            asked_ms,
            drawing: true,
            showing: Showing::default(),
            held: Vec::new(),
        })
    }

    /// Take every whole line in `bytes` that goes on the screen.
    ///
    /// **Every program's line but this program's own**, and **the kernel's
    /// records of the boot before this program asked** — the boot so far, as
    /// the panel it took over would have shown it. A record after that is not
    /// drawn: it would put a `spawn:` and an `exit:` beside every command
    /// typed, as this program's own lines would put a grab and a release beside
    /// every program that takes the keyboard.
    ///
    /// Each is drawn as [`Showing`] shows it.
    fn take(&mut self, bytes: &[u8]) {
        let (asked_ms, drawing, showing, held) =
            (self.asked_ms, &mut self.drawing, &mut self.showing, &mut self.held);
        self.lines.push(bytes, |line, _| {
            let line = String::from_utf8_lossy(line);
            let shown = showing.line(&line);
            let keep = match shown.head.map(|head| head.source) {
                Some(Source::Program(tag)) => tag != OWN_TAG,
                // A record with no time was said before the clock started.
                Some(Source::Kernel | Source::Loader) => {
                    *drawing =
                        toyos_logstream::parse(&line).is_some_and(|p| p.ms.is_none_or(|ms| ms < asked_ms));
                    *drawing
                }
                None => *drawing,
            };
            if keep {
                held.extend_from_slice(format!("{shown}\n").as_bytes());
            }
        });
    }

    /// Draw what is held.
    ///
    /// **Above the shell's unfinished line, never inside it**: where the shell
    /// is part of the way through one — its prompt, and what is being typed at
    /// it — that row is cleared, the lines drawn, and the shell's line drawn
    /// again under them. `partial` is that line as the shell wrote it.
    fn draw(&mut self, partial: &[u8], console: &mut Console) {
        if self.held.is_empty() {
            return;
        }
        if !partial.is_empty() {
            console.write_bytes(b"\r\x1b[K");
        }
        console.write_bytes(&self.held);
        console.write_bytes(partial);
        self.held.clear();
    }
}

fn main() {
    // This console *is* the root of its surface tree — there is no compositor
    // in this image — so it both owns the translator and serves the channel a
    // child asks for raw keys on.
    // Its own port, one per instance, whose connector goes into the namespace
    // of the shell it spawns and nowhere else.
    let (acceptor, connector) =
        port::create().expect("console: the kernel refused a port of its own");
    let mut host = Host::serve(acceptor);
    let mut translator = window::configured_translator();

    // Spawned first so it initialises while the font loads, as `/system/bin/terminal`
    // does.
    let mut shell = Shell::spawn(&connector);

    let Some(fb_dev) = endow::device::<FramebufferDev>(DeviceType::Framebuffer) else {
        // The same answer soundserver and netstack give for their absent device: a
        // console with no screen has nothing to report a failure *to*, and a
        // panic here would replace the boot log with a crash report.
        eprintln!("console: no framebuffer, exiting");
        return;
    };
    let scanout = fb_dev.open().expect("console: the framebuffer claim's description and scanout");
    let info = *scanout.info();
    let screen = Screen::new(
        scanout,
        info.width as usize,
        info.height as usize,
        info.stride as usize,
        info.pixel_format,
    );

    let font_data = std::fs::read(FONT).expect("console: failed to read the font");
    let font = font::Font::from_prebuilt(&font_data);
    let rows = info.height as usize / font.height();
    let cols = info.width as usize / font.width();
    // One row of overlap, so a paged-back screen still shares a line with the
    // one before it and the reader can tell where he is.
    let page_rows = rows.saturating_sub(1);
    let mut console = Console::new(screen, font);

    let mut log = Log::subscribe();
    let seeded = match &mut log {
        Ok(log) => seed(log, &mut console),
        Err(why) => {
            console.write_bytes(format!("[console] no log to show: {why}\n\n").as_bytes());
            0
        }
    };

    // The `input` port and its sources, merged. Their pointer reports are
    // read and moved by nothing: this program draws no pointer.
    let inputs = endow::acceptor(wire::PORT).expect("the manifest declares this program serves `input`");
    let mut sources: [Option<(Connection, FrameRx<MAX_PAYLOAD>)>; CONNECTIONS] = Default::default();
    let mut merge = Merge::default();
    // What the panel cost, on a machine whose only instrument is the panel.
    // The seed is the heaviest thing this program ever draws — a screenful of
    // log per scrolled row — so a boot that felt slow says so here.
    let (panel_bytes, blits) = console.screen_traffic();
    println!(
        "console: ready {}x{} ({cols}x{rows} cells), log {seeded} bytes, \
         panel {panel_bytes} bytes in {blits} blits",
        info.width, info.height
    );

    // The declared set: the shell's two output pipes, the log,
    // this console's own surface listener and its clients, and the `input`
    // port's listener and sources.
    let poller = Poller::new(3 + Host::POLL_HANDLES + 1 + CONNECTIONS as u32);
    const TOKEN_STDOUT: u64 = 0;
    const TOKEN_STDERR: u64 = 1;
    const TOKEN_LISTEN: u64 = 3;
    const TOKEN_CLIENT: u64 = 4;
    const TOKEN_LOG: u64 = 5;
    const TOKEN_INPUT: u64 = 6;
    /// Source `n`'s is this plus `n`.
    const TOKEN_SOURCE: u64 = 7;

    // The shell's unfinished line — a prompt, and what is being typed at it —
    // as it wrote it, for a line of the log to be drawn above.
    let mut partial: Vec<u8> = Vec::new();
    loop {
        poller.watch(&shell.stdout, READABLE, TOKEN_STDOUT);
        poller.watch(&shell.stderr, READABLE, TOKEN_STDERR);
        poller.watch(&host.acceptor_handle(), READABLE, TOKEN_LISTEN);
        for client in host.client_handles() {
            poller.watch(&client, READABLE, TOKEN_CLIENT);
        }
        if let Ok(log) = &log {
            poller.watch(&log.pipe, READABLE, TOKEN_LOG);
        }
        poller.watch(&inputs, READABLE, TOKEN_INPUT);
        for (n, source) in sources.iter().enumerate() {
            if let Some((conn, _)) = source {
                poller.watch(conn, READABLE, TOKEN_SOURCE + n as u64);
            }
        }

        let mut ready = [false; TOKEN_SOURCE as usize + CONNECTIONS];
        poller.wait(1, u64::MAX, |token| {
            if (token as usize) < ready.len() {
                ready[token as usize] = true;
            }
        });

        if ready[TOKEN_STDOUT as usize] {
            let mut buf = [0u8; 4096];
            match shell.stdout.read(&mut buf).unwrap_or(0) {
                0 => {
                    // A machine whose only console has exited is a machine that
                    // needs a reboot to be asked anything, which is the state
                    // this program exists to get out of. `exit` at the prompt
                    // is an ordinary thing to type.
                    shell.restart(&connector);
                    console.write_bytes(b"\n[console] the shell exited; a new one is running\n");
                    partial.clear();
                }
                n => {
                    console.write_bytes(&buf[..n]);
                    unfinished(&mut partial, &buf[..n]);
                }
            }
        }

        if ready[TOKEN_STDERR as usize] {
            let mut buf = [0u8; 4096];
            let n = shell.stderr.read(&mut buf).unwrap_or(0);
            if n > 0 {
                console.write_bytes(&buf[..n]);
                unfinished(&mut partial, &buf[..n]);
            }
        }

        if ready[TOKEN_LOG as usize] {
            if let Ok(reader) = &mut log {
                let mut buf = [0u8; 4096];
                match reader.pipe.read_nonblock(&mut buf) {
                    Ok(0) => {
                        console.write_bytes(b"\n[console] logkeeper stopped writing the log\n");
                        log = Err("logkeeper stopped".to_string());
                    }
                    Ok(n) => reader.take(&buf[..n]),
                    Err(SyscallError::WouldBlock) => {}
                    Err(e) => panic!("console: the log's pipe refused a read: {e:?}"),
                }
            }
        }
        if let Ok(reader) = &mut log {
            reader.draw(&partial, &mut console);
        }

        if ready[TOKEN_LISTEN as usize] {
            host.accept();
        }

        while let Some(notice) = host.poll() {
            match notice {
                // The root of this tree: nothing above to tell, so the re-read
                // happens here and is passed down to every other client.
                Notice::LayoutChanged => {
                    window::load_layout(&mut translator);
                    host.notify_layout();
                    println!("console: keyboard layout is now {}", translator.layout());
                }
                Notice::Grabbed { client } => {
                    println!("console: client {client} has the keyboard until it exits")
                }
                Notice::Released { client } => {
                    println!("console: client {client} gave the keyboard back")
                }
                Notice::Dropped { client, why } => {
                    eprintln!("console: dropping client {client} — {why}")
                }
                // What Ctrl+C reaches; nothing to say about it.
                Notice::Foreground { .. } => {}
            }
        }

        // Every transition of the merged keyboard this turn made, in order.
        let mut keys: Vec<RawKeyEvent> = Vec::new();
        if ready[TOKEN_INPUT as usize] {
            match inputs.accept() {
                Err(e) => eprintln!("console: an input source could not be accepted ({e:?})"),
                Ok(conn) => match sources.iter().position(Option::is_none) {
                    None => eprintln!("console: refusing an input source — {CONNECTIONS} are connected"),
                    Some(n) => sources[n] = Some((conn, FrameRx::new())),
                },
            }
        }
        for n in 0..CONNECTIONS {
            if !ready[TOKEN_SOURCE as usize + n] {
                continue;
            }
            let slot = Slot::connection(n).expect("a source's index is a slot");
            let Some((conn, rx)) = &mut sources[n] else { continue };
            let mut ended = None;
            // A bounded turn, so a source with a frame always ready cannot
            // keep the shell's output from being drawn.
            for _ in 0..SOURCE_FRAMES {
                let record = match rx.pump(conn) {
                    RxStep::Idle => break,
                    RxStep::Eof => {
                        ended = Some("its connection ended".to_string());
                        break;
                    }
                    RxStep::Malformed => {
                        ended = Some("a frame no protocol here can make".to_string());
                        break;
                    }
                    RxStep::Frame { msg_type, payload_len, .. } => wire::decode(msg_type, rx.payload(payload_len)),
                };
                match record {
                    Ok(Record::Key { source, key }) => match merge.key(slot, source, key) {
                        Ok(delivered) => keys.extend(delivered),
                        Err(full) => {
                            ended = Some(format!("{full:?}: more than {DEVICES} of its devices hold something"));
                            break;
                        }
                    },
                    Ok(Record::Pointer(_)) => {}
                    Ok(Record::Gone { source }) => keys.extend(merge.gone(slot, source)),
                    Err(why) => {
                        ended = Some(format!("a record refused: {why:?}"));
                        break;
                    }
                }
            }
            if let Some(why) = ended {
                eprintln!("console: dropping input source {n} — {why}");
                sources[n] = None;
                keys.extend(merge.end(slot));
            }
        }
        for event in keys {
            // A client that asked for the keys gets the transition whole,
            // releases included, and the translator is left where it is.
            if host.deliver(event) == Delivery::Sent {
                continue;
            }
            if !event.pressed() {
                continue;
            }
            match event.keycode {
                // Unchorded, unlike a windowed terminal's Shift+PageUp:
                // nothing in this image reads PageUp, and most of what the
                // panel has to show is the kernel log seeded above the
                // prompt. A scrollback that needs a chord is one the owner
                // does not reach for with a laptop in his hands.
                KEY_PAGE_UP => {
                    console.scroll_view_up(page_rows);
                }
                KEY_PAGE_DOWN => {
                    console.scroll_view_down(page_rows);
                }
                usage => {
                    let text = translator.press(usage, window::KeyEvent::from(event).mods());
                    // Ctrl+C is the foreground line's interrupt, and its
                    // byte only when no line is held.
                    if text.as_bytes() == b"\x03" && host.interrupt() {
                        continue;
                    }
                    if !text.is_empty() {
                        shell.stdin.write_all(text.as_bytes()).ok();
                    }
                }
            }
        }
    }
}

/// The most of the shell's unfinished line kept to draw again: a row and more
/// of any panel, and a bound on output that never ends a line.
const PARTIAL_MAX: usize = 1024;

/// What of the shell's output is still an unfinished line after `bytes`.
fn unfinished(partial: &mut Vec<u8>, bytes: &[u8]) {
    match bytes.iter().rposition(|&b| b == b'\n') {
        Some(at) => {
            partial.clear();
            partial.extend_from_slice(&bytes[at + 1..]);
        }
        None => partial.extend_from_slice(bytes),
    }
    let over = partial.len().saturating_sub(PARTIAL_MAX);
    partial.drain(..over);
}

/// Draw the boot so far — the bytes `logkeeper` handed over with the pipe — before
/// the first prompt; returns the bytes drawn. Every later line arrives on the
/// same pipe and is drawn as it comes.
///
/// Read whole and drawn from its tail ([`seed_tail`]): what falls past the
/// scrollback costs a scroll to draw and is then thrown away.
fn seed(log: &mut Log, console: &mut Console) -> usize {
    let mut boot = Vec::new();
    let mut buf = [0u8; 4096];
    while (boot.len() as u64) < log.handed {
        let want = (log.handed - boot.len() as u64).min(buf.len() as u64) as usize;
        match log.pipe.read(&mut buf[..want]) {
            Ok(0) | Err(_) => break,
            Ok(n) => boot.extend_from_slice(&buf[..n]),
        }
    }
    let tail = seed_tail(&boot);
    log.take(tail);
    log.draw(&[], console);
    tail.len()
}


/// The tail of `log` worth rendering.
///
/// `Console` keeps [`terminal::console::SCROLLBACK_ROWS`] rows and drops what
/// falls past them as it arrives, so an older line costs one full-screen scroll
/// to draw and is then thrown away. A line that wraps takes more than one row,
/// so this is a ceiling on what survives rather than an estimate of it.
fn seed_tail(log: &[u8]) -> &[u8] {
    let window = &log[log.len().saturating_sub(SEED_MAX_BYTES)..];
    let mut newlines = 0;
    for (i, &b) in window.iter().enumerate().rev() {
        if b == b'\n' {
            newlines += 1;
            if newlines > terminal::console::SCROLLBACK_ROWS {
                return &window[i + 1..];
            }
        }
    }
    window
}

struct Shell {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    stderr: ChildStderr,
}

impl Shell {
    fn spawn(surface: &Connector) -> Shell {
        // `[programs.shell]`'s row plus this console's surface, which is the
        // one name no manifest can carry: there is a port per console instance.
        let surface_copy = surface
            .duplicate()
            .expect("console: the kernel refused a duplicate of its own surface connector");
        let mut child = Command::new("/system/bin/shell")
            .independent()
            .provide(surface::SERVICE, surface_copy.into())
            .stdin(process::tty_piped())
            .stdout(process::tty_piped())
            .stderr(process::tty_piped())
            .spawn()
            .expect("console: failed to spawn /system/bin/shell");
        Shell {
            stdin: child.stdin.take().expect("console: shell stdin"),
            stdout: child.stdout.take().expect("console: shell stdout"),
            stderr: child.stderr.take().expect("console: shell stderr"),
            child,
        }
    }

    fn restart(&mut self, surface: &Connector) {
        self.child.wait().ok();
        *self = Shell::spawn(surface);
    }
}
