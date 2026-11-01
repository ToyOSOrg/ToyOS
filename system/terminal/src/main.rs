//! A shell in a window: the compositor's keys in, the emulator's pixels out.
//!
//! It is a surface in both directions. Above it the compositor forwards whole
//! key transitions and `window::Window` holds this process's translator;
//! below it `toyos::surface::Host` lets a child ask for the transitions
//! instead of the bytes, which is what `locale detect` needs and what a
//! terminal writing only translated bytes into a pipe could never give it.
//!
//! **Ctrl+C is the foreground line's interrupt** ([`Host::interrupt`]), and
//! the bytes it would have been only when no line is held. Closing the window,
//! or a hang-up or a terminate of the terminal itself, asks the shell's tree
//! to hang up, and the terminal ends once the shell has. **No timer ends what
//! does not**: the terminal says what it is still waiting for, and only a
//! person's explicit force ends the rest.

use std::io::{Read, Write};
use std::os::toyos::io::AsRawHandle;
use std::os::toyos::process;
use std::os::toyos::process::CommandExt;
use std::process::Command;
use std::time::Duration;

use terminal::Console;
use toyos::poller::{wait_one, Poller, READABLE};
use toyos::port;
use toyos::surface::{self, Delivery, Host, Leaving, Notice};
use window::Window;

const TOKEN_STDOUT: u64 = 0;
const TOKEN_STDERR: u64 = 1;
const TOKEN_WINDOW: u64 = 2;
const TOKEN_LISTEN: u64 = 3;
const TOKEN_CLIENT: u64 = 4;
const TOKEN_NOTICE: u64 = 5;

/// How often the terminal says it still waits for the shell after its
/// hang-up: a report of a stall, never a deadline.
const STILL_WAITING: Duration = Duration::from_secs(10);

/// The desktop's monospace font, which the terminal draws at its window's
/// scale.
const FONT: &str = "/system/share/fonts/JetBrainsMono-Regular.ttf";

/// The font at `scale`'s text cell.
fn font_at(ttf: &[u8], scale: window::Scale) -> font::Font {
    let (w, h) = scale.cell();
    font::Font::from_ttf(ttf, w, h).unwrap_or_else(|e| panic!("terminal: {FONT}: {e}"))
}

/// Give the compositor the cells the emulator just repainted, and nothing when
/// it repainted none.
///
/// The whole of why a typed character does not cost a window repaint: the
/// emulator already blits one cell into the shared buffer, and before this the
/// compositor was told only that *something* had changed and had to assume all
/// of it had.
fn present(console: &Console, window: &Window) {
    if let Some(damage) = console.take_damage() {
        window.present_damage(damage);
    }
}

fn copy(text: &str) {
    if let Err(e) = window::clipboard_set(text) {
        eprintln!("terminal: nothing was copied — {e}");
    }
}

fn main() {
    // **This terminal's surface is a port it makes, not a name it registers.**
    // One per instance: the connector goes into the namespace of the shell it
    // spawns and nowhere else, so nothing outside that subtree can reach this
    // terminal's keyboard and nothing inside it can name a service this
    // process was not given. `TOYOS_SURFACE` — an ambient string naming a
    // machine-wide service — is what that replaces.
    let (acceptor, connector) =
        port::create().expect("terminal: the kernel refused a port of its own");
    let mut host = Host::serve(acceptor);

    // Spawn shell first so it initializes while we load the font
    let mut child = shell(&connector);

    let mut window = Window::create_with_title(window::Size::Any, "Terminal").unwrap_or_else(|e| {
        eprintln!("terminal: {e}");
        std::process::exit(1);
    });
    let ttf = std::fs::read(FONT).unwrap_or_else(|e| panic!("terminal: {FONT}: {e}"));
    let mut scale = window.scale();
    let mut console = Console::new(window.screen(), font_at(&ttf, scale));

    let mut shell_stdin = child.stdin.take().unwrap();
    let mut shell_stdout = child.stdout.take().unwrap();
    let mut shell_stderr = child.stderr.take().unwrap();
    let poller = Poller::new(4 + Host::POLL_HANDLES);
    let notice = toyos::quit::notice();

    // The window exists and the shell's stdin is a pipe this process owns, so
    // from here a keystroke the compositor forwards has somewhere to land even
    // if the shell has not reached its first read. Before it, one is dropped
    // with no trace.
    println!("terminal: ready");

    let leaving = loop {
        poller.watch(&shell_stdout, READABLE, TOKEN_STDOUT);
        poller.watch(&shell_stderr, READABLE, TOKEN_STDERR);
        poller.watch(&window.handle(), READABLE, TOKEN_WINDOW);
        poller.watch(&host.acceptor_handle(), READABLE, TOKEN_LISTEN);
        for client in host.client_handles() {
            poller.watch(&client, READABLE, TOKEN_CLIENT);
        }
        if let Some(notice) = notice {
            poller.watch(notice, READABLE, TOKEN_NOTICE);
        }

        let mut ready = [false; 6];
        poller.wait(1, u64::MAX, |token| {
            if (token as usize) < ready.len() { ready[token as usize] = true; }
        });

        if ready[TOKEN_STDOUT as usize] {
            let mut buf = [0u8; 4096];
            let n = shell_stdout.read(&mut buf).unwrap_or(0);
            if n == 0 {
                // The child closes every fd together, so a last line it wrote
                // to stderr right before exiting is already sitting in that
                // pipe — drained here or it is lost with the loop.
                let n = shell_stderr.read(&mut buf).unwrap_or(0);
                if n > 0 {
                    console.write_bytes(&buf[..n]);
                    present(&console, &window);
                }
                break Leaving::ShellEnded;
            }
            console.write_bytes(&buf[..n]);
            present(&console, &window);
        }

        if ready[TOKEN_STDERR as usize] {
            let mut buf = [0u8; 4096];
            let n = shell_stderr.read(&mut buf).unwrap_or(0);
            if n > 0 {
                console.write_bytes(&buf[..n]);
                present(&console, &window);
            }
        }

        if ready[TOKEN_LISTEN as usize] {
            host.accept();
        }

        // An interrupt is the foreground's, which the quit that asked it
        // reached already; a hang-up or a terminate closes this terminal.
        if ready[TOKEN_NOTICE as usize] {
            let asked = notice.and_then(|n| n.try_read().ok().flatten());
            if let Some(reason @ (toyos::quit::Reason::HangUp | toyos::quit::Reason::Terminate)) = asked {
                break Leaving::Asked(reason);
            }
        }

        while let Some(notice) = host.poll() {
            match notice {
                // Up, not down: the compositor is the root of the tree and
                // broadcasts to every window, this one included, so the
                // re-read arrives back through `Event::LayoutChanged`.
                Notice::LayoutChanged => window.notify_layout_changed(),
                Notice::Grabbed { client } => {
                    println!("terminal: client {client} has the keyboard until it exits")
                }
                Notice::Released { client } => {
                    println!("terminal: client {client} gave the keyboard back")
                }
                Notice::Dropped { client, why } => {
                    eprintln!("terminal: dropping client {client} — {why}")
                }
                // What Ctrl+C reaches; nothing to say about it.
                Notice::Foreground { .. } => {}
            }
        }

        if ready[TOKEN_WINDOW as usize] {
            match window.recv_event() {
                // A client holding the grab takes the transition whole, and
                // the translator is not advanced — see `Window::text`.
                window::Event::KeyInput(key) if host.deliver(key.into()) == Delivery::Sent => {}
                window::Event::KeyInput(key) if key.pressed() && key.gui() && key.keycode == 0x06 => {
                    if let Some(text) = console.get_selection() {
                        copy(&text);
                    }
                }
                window::Event::KeyInput(key) => {
                    let press = window.press(key);
                    if press.text() == "\x03" && host.interrupt() {
                        continue;
                    }
                    if !press.text().is_empty() {
                        shell_stdin.write_all(press.text().as_bytes()).ok();
                    }
                }
                window::Event::LayoutChanged => host.notify_layout(),
                window::Event::ClipboardPaste(data) => {
                    shell_stdin.write_all(&data).ok();
                }
                window::Event::MouseInput(ev) => {
                    let col = ev.x as usize / console.font_width();
                    let row = ev.y as usize / console.font_height();
                    match ev.event_type {
                        window::MOUSE_PRESS if ev.changed == 1 => {
                            console.mouse_down(col, row);
                            present(&console, &window);
                        }
                        window::MOUSE_MOVE if ev.buttons & 1 != 0 => {
                            console.mouse_drag(col, row);
                            present(&console, &window);
                        }
                        window::MOUSE_RELEASE if ev.changed == 1 => {
                            if let Some(text) = console.mouse_up(col, row) {
                                copy(&text);
                            }
                            present(&console, &window);
                        }
                        window::MOUSE_SCROLL => {
                            if ev.scroll < 0 {
                                console.scroll_view_up(1);
                            } else if ev.scroll > 0 {
                                console.scroll_view_down(1);
                            }
                            present(&console, &window);
                        }
                        _ => {}
                    }
                }
                window::Event::Close => break Leaving::Closed,
                window::Event::Resized => {
                    let font = (window.scale() != scale).then(|| font_at(&ttf, window.scale()));
                    scale = window.scale();
                    console.resize(window.screen(), font);
                    present(&console, &window);
                }
                window::Event::Frame => {}
            }
        }
    };

    drop(window);
    if let Some(reason) = leaving.asks() {
        let _ = toyos_abi::syscall::process_quit(toyos::RawHandle(child.as_raw_handle()), reason);
    }
    wait_for(&mut child);
    drop(shell_stdin);
    drop(shell_stdout);
    drop(shell_stderr);
}

/// Wait for the shell to end, saying every [`STILL_WAITING`] how long it has
/// been outstanding.
fn wait_for(child: &mut std::process::Child) {
    let mut waited = Duration::ZERO;
    loop {
        let ended = wait_one(&*child, READABLE, STILL_WAITING.as_nanos() as u64)
            .expect("terminal: the kernel refused a wait on the shell it started");
        if ended {
            return;
        }
        waited += STILL_WAITING;
        eprintln!("terminal: the shell (pid {}) has not ended {}s after this terminal left it; still waiting", child.id(), waited.as_secs());
    }
}

/// Start the shell: `[programs.shell]`'s own row, plus this terminal's surface.
///
/// **The row is the supervisor's to build and the surface is this terminal's to give.**
/// A shell's authority is a decision the manifest makes, and until the launcher
/// existed a terminal could hand a child only what it held itself — so it
/// handed over a hand-written union of its own names. `provide` is the shape
/// that replaces it: the one connector no manifest can name travels from here,
/// everything else comes from the declaration, and a name this terminal happens
/// to hold is no longer a name its shell inherits.
fn shell(surface: &toyos::port::Connector) -> std::process::Child {
    let handed = surface
        .duplicate()
        .expect("terminal: the kernel refused a duplicate of its own surface connector");
    Command::new("/system/bin/shell")
        .independent()
        .provide(surface::SERVICE, handed.into())
        .stdin(process::tty_piped())
        .stdout(process::tty_piped())
        .stderr(process::tty_piped())
        .spawn()
        .expect("failed to spawn shell")
}
