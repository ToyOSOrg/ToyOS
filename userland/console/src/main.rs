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
//!   the diagnostic that works today for one that might. It asks `logd` for the
//!   log ([`toyos_logstream::SERVICE`]) and draws the boot so far above the
//!   first prompt — the kernel's records and every program's output — and
//!   every program's line after as it is written, each under its program's
//!   name, between the shell's lines (`Log::take`).
//! - **A fatal panic still takes the screen back.** `render` ignores
//!   `SCREEN_OWNED_BY_USERLAND` entirely — only boot checkpoints honour it —
//!   so the report paints over whatever this program drew.
//!   `screen_console_panic` is the gate.
//! - **The emulator is `/system/bin/terminal`'s**, unchanged. `Console::new` always
//!   took a raw mapping; the compositor was never below it. This is the caller
//!   whose mapping is the scanout, so it is the one that pays for a read.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::toyos::process;
use std::os::toyos::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

use terminal::Console;
use toyos::poller::{Poller, READABLE};
use toyos::shm::SharedMemory;
use toyos::endow;
use toyos::port::{self, Connector};
use toyos::surface::{self, Delivery, Host, Notice};
use toyos::{FramebufferDev, Keyboard, Pipe};
use toyos_abi::syscall::{DeviceType, SyscallError};
use toyos_logstream::{Lines, SERVED, SERVICE};
use window::Screen;

const FONT: &str = "/system/share/fonts/JetBrainsMono-Regular-8x16.font";

/// HID usage codes. `toyos_keymap::Translator` turns both into escape
/// sequences; this program consumes them before it asks.
const KEY_PAGE_UP: u8 = 0x4B;
const KEY_PAGE_DOWN: u8 = 0x4E;

/// The most of the boot so far [`seed_tail`] will look at.
///
/// **A bound on this program's work.** The scrollback bound below is what
/// normally decides how far back the seed goes; this one is what stops a log
/// with no newlines in it from being walked end to end before that bound can
/// apply.
const SEED_MAX_BYTES: usize = 64 * 1024;

/// The name `/system/bin/init` starts this program under, which its own lines
/// come back from `logd` tagged with.
const OWN_TAG: &str = "console";

/// This boot's log as `logd` hands it to a reader on this machine: a pipe the
/// log is written into, and how many of its bytes are the boot so far.
struct Log {
    pipe: Pipe,
    lines: Lines,
    handed: u64,
    /// When this program asked, in milliseconds since boot: a kernel record
    /// stamped before it is the boot so far, however late `logd` read it.
    asked_ms: u64,
    /// Whether the last kernel record was kept, which its continuation lines
    /// follow.
    drawing: bool,
    /// Lines kept and not yet drawn: they wait while the shell is part of the
    /// way through a line, so a line of the log never lands inside a prompt
    /// and what is being typed at it.
    held: Vec<u8>,
}

impl Log {
    /// Ask `logd`. A blocking read of one frame from the server this image
    /// names, which answers as it accepts.
    fn subscribe() -> Result<Self, String> {
        let asked_ms = toyos_abi::syscall::clock_nanos() / 1_000_000;
        let conn = endow::service(SERVICE).map_err(|e| format!("no `{SERVICE}` service: {e:?}"))?;
        let header = conn.recv_header().map_err(|e| format!("logd did not answer: {e:?}"))?;
        if header.msg_type != SERVED {
            return Err(format!("logd answered frame type {}", header.msg_type));
        }
        let handed: u64 =
            conn.recv_payload(&header).map_err(|e| format!("logd's answer is short: {e:?}"))?;
        let [raw] = conn.recv_handles_exact::<1>().ok_or("logd's answer carried no pipe")?;
        // SAFETY: the kernel moved this handle into this table with the frame
        // that names it, and nothing else answers for it.
        let pipe = unsafe { Pipe::from_raw(raw) };
        Ok(Self { pipe, lines: Lines::new(), handed, asked_ms, drawing: true, held: Vec::new() })
    }

    /// Take every whole line in `bytes` that goes on the screen.
    ///
    /// **Every program's line but this program's own**, which it has already
    /// drawn, and **the kernel's records of the boot before this program
    /// asked** — the boot so far, as the panel it took over would have shown
    /// it. A record after that is not drawn: it would put a `spawn:` and an
    /// `exit:` beside every command typed.
    fn take(&mut self, bytes: &[u8]) {
        let (asked_ms, drawing, held) = (self.asked_ms, &mut self.drawing, &mut self.held);
        self.lines.push(bytes, |line| {
            let text = std::str::from_utf8(line).ok();
            let keep = match text.and_then(toyos_logstream::program_line) {
                Some(said) => said.tag != OWN_TAG,
                None => {
                    if let Some(ms) = text.and_then(toyos_logstream::record_ms) {
                        *drawing = ms < asked_ms;
                    }
                    *drawing
                }
            };
            if keep {
                held.extend_from_slice(line);
                held.push(b'\n');
            }
        });
    }

    /// Draw what is held; whether there was any.
    fn draw(&mut self, console: &mut Console) -> bool {
        if self.held.is_empty() {
            return false;
        }
        console.write_bytes(&self.held);
        self.held.clear();
        true
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
        // The same answer soundd and netd give for their absent device: a
        // console with no screen has nothing to report a failure *to*, and a
        // panic here would replace the boot log with a crash report.
        eprintln!("console: no framebuffer, exiting");
        return;
    };
    let info = fb_dev.info().expect("console: framebuffer info");
    let shm = SharedMemory::adopt(info.scanout[0], info.stride as usize * info.height as usize * 4)
        .expect("console: the scanout buffer the framebuffer claim just handed over");
    let screen = Screen::new(
        shm.as_ptr(),
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
    present(&fb_dev, info.width, info.height);

    let kb: Keyboard = endow::device(DeviceType::Keyboard)
        .expect("the manifest gives this program the keyboard");
    // What the panel cost, on a machine whose only instrument is the panel.
    // The seed is the heaviest thing this program ever draws — a screenful of
    // log per scrolled row — so a boot that felt slow says so here.
    let (panel_bytes, blits) = console.screen_traffic();
    eprintln!(
        "console: ready {}x{} ({cols}x{rows} cells), log {seeded} bytes, \
         panel {panel_bytes} bytes in {blits} blits",
        info.width, info.height
    );

    // The declared set: the shell's two output pipes, the keyboard, the log,
    // and this console's own surface listener and its clients.
    let poller = Poller::new(4 + Host::POLL_HANDLES);
    const TOKEN_STDOUT: u64 = 0;
    const TOKEN_STDERR: u64 = 1;
    const TOKEN_KEYBOARD: u64 = 2;
    const TOKEN_LISTEN: u64 = 3;
    const TOKEN_CLIENT: u64 = 4;
    const TOKEN_LOG: u64 = 5;

    // Whether the shell's last byte left a line unfinished — a prompt, or what
    // is being typed at it.
    let mut mid_line = false;
    loop {
        poller.watch_raw(toyos::RawHandle(shell.stdout.as_raw_fd() as u32), READABLE, TOKEN_STDOUT);
        poller.watch_raw(toyos::RawHandle(shell.stderr.as_raw_fd() as u32), READABLE, TOKEN_STDERR);
        poller.watch(&kb, READABLE, TOKEN_KEYBOARD);
        poller.watch_raw(host.acceptor_handle(), READABLE, TOKEN_LISTEN);
        for client in host.client_handles() {
            poller.watch_raw(client, READABLE, TOKEN_CLIENT);
        }
        if let Ok(log) = &log {
            poller.watch(&log.pipe, READABLE, TOKEN_LOG);
        }

        let mut ready = [false; 6];
        poller.wait(1, u64::MAX, |token| {
            if (token as usize) < ready.len() {
                ready[token as usize] = true;
            }
        });

        let mut painted = false;

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
                    mid_line = false;
                    painted = true;
                }
                n => {
                    console.write_bytes(&buf[..n]);
                    std::io::stdout().lock().write_all(&buf[..n]).ok();
                    mid_line = buf[n - 1] != b'\n';
                    painted = true;
                }
            }
        }

        if ready[TOKEN_STDERR as usize] {
            let mut buf = [0u8; 4096];
            let n = shell.stderr.read(&mut buf).unwrap_or(0);
            if n > 0 {
                console.write_bytes(&buf[..n]);
                std::io::stdout().lock().write_all(&buf[..n]).ok();
                mid_line = buf[n - 1] != b'\n';
                painted = true;
            }
        }

        if ready[TOKEN_LOG as usize] {
            if let Ok(reader) = &mut log {
                let mut buf = [0u8; 4096];
                match reader.pipe.read_nonblock(&mut buf) {
                    Ok(0) => {
                        console.write_bytes(b"\n[console] logd stopped writing the log\n");
                        log = Err("logd stopped".to_string());
                    }
                    Ok(n) => reader.take(&buf[..n]),
                    Err(SyscallError::WouldBlock) => {}
                    Err(e) => panic!("console: the log's pipe refused a read: {e:?}"),
                }
            }
        }
        // Between the shell's lines only, and a line the shell ends lets what
        // waited through it be drawn.
        if let (false, Ok(reader)) = (mid_line, &mut log) {
            painted |= reader.draw(&mut console);
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
                    eprintln!("console: keyboard layout is now {}", translator.layout());
                }
                Notice::Grabbed { client } => {
                    eprintln!("console: client {client} has the keyboard until it exits")
                }
                Notice::Released { client } => {
                    eprintln!("console: client {client} gave the keyboard back")
                }
                Notice::Dropped { client, why } => {
                    eprintln!("console: dropping client {client} — {why}")
                }
            }
        }

        if ready[TOKEN_KEYBOARD as usize] {
            let mut events = [toyos_abi::input::RawKeyEvent { keycode: 0, modifiers: 0 }; 16];
            let buf = unsafe {
                std::slice::from_raw_parts_mut(
                    events.as_mut_ptr() as *mut u8,
                    std::mem::size_of_val(&events),
                )
            };
            // Non-blocking for the reason `Keyboard::read_nonblock` documents:
            // an event loop that can park on an empty queue is a frozen screen.
            let n = kb.read_nonblock(buf).unwrap_or(0);
            for &event in &events[..n / std::mem::size_of::<toyos_abi::input::RawKeyEvent>()] {
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
                        painted = true;
                    }
                    KEY_PAGE_DOWN => {
                        console.scroll_view_down(page_rows);
                        painted = true;
                    }
                    usage => {
                        let text = translator.press(usage, window::KeyEvent::from(event).mods());
                        if !text.is_empty() {
                            shell.stdin.write_all(text.as_bytes()).ok();
                        }
                    }
                }
            }
        }

        if painted {
            present(&fb_dev, info.width, info.height);
        }
    }
}

/// Draw the boot so far — the bytes `logd` handed over with the pipe — before
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
    log.draw(console);
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

/// One present per drained batch, never per byte. Free on a GOP framebuffer —
/// `gop.rs`'s `present_rect` is empty, because the scanout *is* the memory just
/// written — and one transfer per batch on virtio-gpu, the only backend where
/// it costs anything.
fn present(fb: &FramebufferDev, width: u32, height: u32) {
    fb.present(0, 0, width, height).expect("console holds the framebuffer claim");
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
            .provide(surface::SERVICE, surface_copy.into_raw().0)
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
