use filepicker_api::{
    PickerMode, MAX_REQUEST_BYTES, MSG_FILEPICKER_REQUEST, MSG_FILEPICKER_RESULT,
};
use font::Font;
use std::fs;
use std::time::{Duration, Instant};
use toyos::ipc::{self, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::AsHandle;
use toyos::Connection;
use toyos::endow;
use std::path::{Path, PathBuf};
use window::{Color, Event, Framebuffer, KeyPress, MouseEvent, Window};

// --- Colors (matching editor theme) ---

const BG: Color = Color { r: 0x1e, g: 0x1e, b: 0x2e };
const TEXT_FG: Color = Color { r: 0xcd, g: 0xd6, b: 0xf4 };
const DIM_FG: Color = Color { r: 0x6c, g: 0x70, b: 0x86 };
const DIR_FG: Color = Color { r: 0x89, g: 0xb4, b: 0xfa };
const SEL_BG: Color = Color { r: 0x45, g: 0x47, b: 0x5a };
const PATH_BG: Color = Color { r: 0x31, g: 0x32, b: 0x44 };
const INPUT_BG: Color = Color { r: 0x11, g: 0x11, b: 0x1b };
const BUTTON_BG: Color = Color { r: 0x45, g: 0x47, b: 0x5a };
const BUTTON_FG: Color = Color { r: 0xcd, g: 0xd6, b: 0xf4 };
const ACCENT_BG: Color = Color { r: 0x89, g: 0xb4, b: 0xfa };
const ACCENT_FG: Color = Color { r: 0x1e, g: 0x1e, b: 0x2e };
const CURSOR_COLOR: Color = Color { r: 0xf5, g: 0xe0, b: 0xdc };

// --- HID keycodes ---

const KEY_UP: u8 = 0x52;
const KEY_DOWN: u8 = 0x51;
const KEY_LEFT: u8 = 0x50;
const KEY_RIGHT: u8 = 0x4F;
const KEY_BACKSPACE: u8 = 0x2A;
const KEY_ENTER: u8 = 0x28;
const KEY_TAB: u8 = 0x2B;
const KEY_ESCAPE: u8 = 0x29;

// --- Directory entry ---

struct Entry {
    name: String,
    is_dir: bool,
}

fn list_dir(path: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();

    if let Ok(read_dir) = fs::read_dir(path) {
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().map_or(false, |ft| ft.is_dir());
            entries.push(Entry { name, is_dir });
        }
    }

    // Sort: directories first, then alphabetical
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    entries
}

// --- Picker state ---

struct Picker {
    mode: PickerMode,
    current_dir: PathBuf,
    entries: Vec<Entry>,
    selected: usize,
    scroll: usize,
    filename: String,
    filename_cursor: usize,
    focus_filename: bool, // true = filename input focused, false = file list focused
    font_w: usize,
    font_h: usize,
}

impl Picker {
    fn new(mode: PickerMode, start_dir: &str, font_w: usize, font_h: usize) -> Self {
        let current_dir = PathBuf::from(if start_dir.is_empty() { "/" } else { start_dir });
        let entries = list_dir(&current_dir);
        Self {
            mode,
            current_dir,
            entries,
            selected: 0,
            scroll: 0,
            filename: String::new(),
            filename_cursor: 0,
            focus_filename: mode == PickerMode::Save,
            font_w,
            font_h,
        }
    }

    fn refresh(&mut self) {
        self.entries = list_dir(&self.current_dir);
        self.selected = 0;
        self.scroll = 0;
    }

    fn navigate_into(&mut self, dir_name: &str) {
        if dir_name == ".." {
            if let Some(parent) = self.current_dir.parent() {
                self.current_dir = parent.to_path_buf();
            }
        } else {
            self.current_dir.push(dir_name);
        }
        self.refresh();
    }

    fn visible_rows(&self, win_h: usize) -> usize {
        let top = self.font_h + 8; // path bar
        let bottom = if self.mode == PickerMode::Save {
            (self.font_h + 8) * 2 // filename input + action bar
        } else {
            self.font_h + 8 // action bar only
        };
        (win_h.saturating_sub(top + bottom)) / self.font_h
    }

    fn ensure_visible(&mut self, win_h: usize) {
        let vis = self.visible_rows(win_h);
        if vis == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + vis {
            self.scroll = self.selected - vis + 1;
        }
    }

    /// Returns the full path of the result, or None to cancel.
    fn activate(&self) -> PickerAction {
        if self.focus_filename && self.mode == PickerMode::Save {
            if self.filename.is_empty() {
                return PickerAction::None;
            }
            let path = self.current_dir.join(&self.filename);
            return PickerAction::Pick(path.to_string_lossy().into_owned());
        }

        if self.entries.is_empty() {
            return PickerAction::None;
        }

        let entry = &self.entries[self.selected];
        if entry.is_dir {
            PickerAction::EnterDir(entry.name.clone())
        } else {
            let path = self.current_dir.join(&entry.name);
            if self.mode == PickerMode::Save {
                // In save mode, selecting a file populates the filename field
                PickerAction::SetFilename(entry.name.clone())
            } else {
                PickerAction::Pick(path.to_string_lossy().into_owned())
            }
        }
    }
}

enum PickerAction {
    None,
    Pick(String),
    EnterDir(String),
    SetFilename(String),
}

// --- Rendering ---

fn render(fb: &Framebuffer, font: &Font, picker: &Picker) {
    let w = fb.width();
    let h = fb.height();
    let fw = picker.font_w;
    let fh = picker.font_h;

    fb.clear(BG);

    let path_str = picker.current_dir.to_string_lossy();
    fb.fill_rect(0, 0, w, fh + 8, PATH_BG);
    font.draw_string(fb, 8, 4, &path_str, TEXT_FG, PATH_BG);

    let list_y = fh + 8;
    let vis = picker.visible_rows(h);

    for i in 0..vis {
        let idx = picker.scroll + i;
        if idx >= picker.entries.len() {
            break;
        }
        let entry = &picker.entries[idx];
        let y = list_y + i * fh;

        let (bg, fg) = if idx == picker.selected && !picker.focus_filename {
            (SEL_BG, TEXT_FG)
        } else if entry.is_dir {
            (BG, DIR_FG)
        } else {
            (BG, TEXT_FG)
        };

        if idx == picker.selected && !picker.focus_filename {
            fb.fill_rect(0, y, w, fh, SEL_BG);
        }

        let display = if entry.is_dir {
            format!("  {}/", entry.name)
        } else {
            format!("  {}", entry.name)
        };
        font.draw_string(fb, 8, y, &display, fg, bg);
    }

    let mut bottom_y = h;

    // Action bar (always at very bottom)
    bottom_y -= fh + 8;
    let action_y = bottom_y;
    fb.fill_rect(0, action_y, w, fh + 8, PATH_BG);

    let cancel_label = " Cancel ";
    let action_label = if picker.mode == PickerMode::Save {
        " Save "
    } else {
        " Open "
    };

    let action_w = action_label.len() * fw;
    let cancel_w = cancel_label.len() * fw;
    let action_x = w - action_w - 8;
    let cancel_x = action_x - cancel_w - 8;

    fb.fill_rect(cancel_x, action_y + 2, cancel_w, fh + 4, BUTTON_BG);
    font.draw_string(fb, cancel_x, action_y + 4, cancel_label, BUTTON_FG, BUTTON_BG);

    fb.fill_rect(action_x, action_y + 2, action_w, fh + 4, ACCENT_BG);
    font.draw_string(fb, action_x, action_y + 4, action_label, ACCENT_FG, ACCENT_BG);

    // Filename input (Save mode only)
    if picker.mode == PickerMode::Save {
        bottom_y -= fh + 8;
        let input_y = bottom_y;
        fb.fill_rect(0, input_y, w, fh + 8, PATH_BG);

        let label = "Filename: ";
        font.draw_string(fb, 8, input_y + 4, label, DIM_FG, PATH_BG);

        let input_x = 8 + label.len() * fw;
        let input_w = w - input_x - 8;
        fb.fill_rect(input_x, input_y + 2, input_w, fh + 4, INPUT_BG);
        font.draw_string(fb, input_x + 4, input_y + 4, &picker.filename, TEXT_FG, INPUT_BG);

        if picker.focus_filename {
            let cx = input_x + 4 + picker.filename_cursor * fw;
            fb.fill_rect(cx, input_y + 4, 2, fh, CURSOR_COLOR);
        }
    }
}

// --- Event handling ---

enum PickerResult {
    Continue,
    Pick(String),
    Cancel,
}

fn handle_key(picker: &mut Picker, key: &KeyPress, win_h: usize) -> PickerResult {
    if !key.pressed() {
        return PickerResult::Continue;
    }

    match key.keycode {
        KEY_ESCAPE => return PickerResult::Cancel,

        KEY_TAB if picker.mode == PickerMode::Save => {
            picker.focus_filename = !picker.focus_filename;
        }

        KEY_ENTER => {
            match picker.activate() {
                PickerAction::Pick(path) => return PickerResult::Pick(path),
                PickerAction::EnterDir(name) => {
                    picker.navigate_into(&name);
                }
                PickerAction::SetFilename(name) => {
                    picker.filename = name;
                    picker.filename_cursor = picker.filename.len();
                    picker.focus_filename = true;
                }
                PickerAction::None => {}
            }
        }

        KEY_BACKSPACE => {
            if picker.focus_filename {
                if picker.filename_cursor > 0 {
                    picker.filename_cursor -= 1;
                    picker.filename.remove(picker.filename_cursor);
                }
            } else {
                // Go up a directory
                picker.navigate_into("..");
            }
        }

        KEY_UP if !picker.focus_filename => {
            if picker.selected > 0 {
                picker.selected -= 1;
                picker.ensure_visible(win_h);
            }
        }

        KEY_DOWN if !picker.focus_filename => {
            if picker.selected + 1 < picker.entries.len() {
                picker.selected += 1;
                picker.ensure_visible(win_h);
            }
        }

        KEY_LEFT if picker.focus_filename => {
            picker.filename_cursor = picker.filename_cursor.saturating_sub(1);
        }

        KEY_RIGHT if picker.focus_filename => {
            picker.filename_cursor = (picker.filename_cursor + 1).min(picker.filename.len());
        }

        _ => {
            if picker.focus_filename {
                for ch in key.text().chars() {
                    if ch >= ' ' && ch != '/' {
                        picker.filename.insert(picker.filename_cursor, ch);
                        picker.filename_cursor += 1;
                    }
                }
            }
        }
    }

    PickerResult::Continue
}

fn handle_mouse(picker: &mut Picker, mouse: &MouseEvent, win_h: usize) -> PickerResult {
    let fh = picker.font_h;
    let py = mouse.y as usize;

    match mouse.event_type {
        window::MOUSE_PRESS if mouse.changed == 1 => {
            let list_y = fh + 8;
            let vis = picker.visible_rows(win_h);
            let list_end = list_y + vis * fh;

            if py >= list_y && py < list_end {
                let idx = picker.scroll + (py - list_y) / fh;
                if idx < picker.entries.len() {
                    picker.selected = idx;
                    picker.focus_filename = false;
                }
            }

            // Check filename input area click (Save mode)
            if picker.mode == PickerMode::Save {
                let input_y = win_h - (fh + 8) * 2;
                if py >= input_y && py < input_y + fh + 8 {
                    picker.focus_filename = true;
                }
            }
        }

        window::MOUSE_SCROLL => {
            if mouse.scroll < 0 {
                picker.scroll = picker.scroll.saturating_sub(3);
            } else {
                let max_scroll = picker.entries.len().saturating_sub(1);
                picker.scroll = (picker.scroll + 3).min(max_scroll);
            }
        }

        _ => {}
    }

    PickerResult::Continue
}

// --- Run a single file picker session ---

fn run_picker(mode: PickerMode, start_dir: &str, client: &Connection) {
    let title = if mode == PickerMode::Save {
        "Save As"
    } else {
        "Open File"
    };

    // A refusal ends this session, not the daemon: the next request may well
    // arrive after whatever was holding the compositor's windows has exited.
    // The client is answered as if the user cancelled, because from its side
    // that is exactly what happened — no file was picked.
    let mut window = match Window::create_topmost(500, 400, title) {
        Ok(window) => window,
        Err(e) => {
            eprintln!("filepicker: {e}");
            let _ = client.send_bytes(MSG_FILEPICKER_RESULT, &[]);
            return;
        }
    };
    let mut fb = window.framebuffer();

    let font_data = fs::read("/share/fonts/JetBrainsMono-Regular-8x16.font").expect("Failed to load font");
    let font = Font::from_prebuilt(&font_data);

    let mut picker = Picker::new(mode, start_dir, font.width(), font.height());

    render(&fb, &font, &picker);
    window.present();

    loop {
        let event = window.recv_event();
        let mut needs_redraw = true;

        let result = match event {
            Event::Close => PickerResult::Cancel,

            Event::Resized => {
                fb = window.framebuffer();
                PickerResult::Continue
            }

            Event::KeyInput(key) => handle_key(&mut picker, &window.press(key), fb.height()),

            Event::MouseInput(mouse) => handle_mouse(&mut picker, &mouse, fb.height()),

            _ => {
                needs_redraw = false;
                PickerResult::Continue
            }
        };

        match result {
            PickerResult::Pick(path) => {
                let _ = client.send_bytes(MSG_FILEPICKER_RESULT, path.as_bytes());
                return;
            }
            PickerResult::Cancel => {
                let _ = client.send_bytes(MSG_FILEPICKER_RESULT, &[]);
                return;
            }
            PickerResult::Continue => {}
        }

        if needs_redraw {
            render(&fb, &font, &picker);
            window.present();
        }
    }
}

// --- Main daemon loop ---

/// How many connections may be waiting to say what they want. The kernel's own
/// per-port queue depth (`listener::MAX_PENDING_CONNECTIONS`) one step further
/// along; past it the picker refuses by name rather than growing.
const MAX_PENDING_REQUESTS: usize = 32;

/// How long an accepted connection may go without completing its request.
/// Every caller sends its frame in the statement after `connect`, so what this
/// bounds is the one that never sends it, and it is what drains the table.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// One caller's inbound framing.
type RequestRx = ipc::FrameRx<MAX_REQUEST_BYTES>;

/// A connection that has been accepted and has not yet said what it wants.
struct Pending {
    conn: Connection,
    rx: RequestRx,
    since: Instant,
}

const TOKEN_ACCEPTOR: u64 = 0;
const TOKEN_PENDING_BASE: u64 = 1;

/// Serve `filepicker` for the rest of the machine's life.
///
/// **A server never blocks on a client.** Accept and the request frame are two
/// events and a frame is buffered until whole before anything acts on it, so a
/// caller that connects and says nothing costs a slot and a deadline. A picker
/// session is modal and does hold the loop — that is what a dialog is — but it
/// begins only once a whole request has arrived.
fn main() {
    // A statement about the manifest the image was built from, not a race: the
    // `filepicker` port exists before any process does, so an editor holding
    // its connector can ask for a file before this program has run an
    // instruction.
    let acceptor = endow::acceptor("filepicker")
        .expect("the manifest declares this program serves `filepicker`");

    let poller = Poller::new(1 + MAX_PENDING_REQUESTS as u32);
    let mut pending: Vec<Pending> = Vec::new();
    let mut ready: Vec<u64> = Vec::new();
    loop {
        poller.watch(&acceptor, READABLE, TOKEN_ACCEPTOR);
        for p in &pending {
            poller.watch(&p.conn, READABLE, TOKEN_PENDING_BASE + p.conn.as_handle().0 as u64);
        }
        // A client that says nothing wakes nothing, so the wait is what is left
        // of the oldest deadline rather than a fresh one.
        let now = Instant::now();
        let timeout = pending
            .iter()
            .map(|p| HANDSHAKE_TIMEOUT.saturating_sub(now.duration_since(p.since)))
            .min()
            .map_or(u64::MAX, |left| left.as_nanos() as u64);
        ready.clear();
        poller.wait(1, timeout, |token| ready.push(token));

        let now = Instant::now();
        for p in pending.iter().filter(|p| now.duration_since(p.since) >= HANDSHAKE_TIMEOUT) {
            eprintln!(
                "filepicker: dropping client {} — it never finished its request",
                p.conn.as_handle().0
            );
        }
        pending.retain(|p| now.duration_since(p.since) < HANDSHAKE_TIMEOUT);

        if ready.contains(&TOKEN_ACCEPTOR) {
            // An accept that fails costs one connection and never the picker.
            match acceptor.accept() {
                Err(e) => eprintln!("filepicker: a connection could not be accepted ({e:?})"),
                Ok(conn) if pending.len() >= MAX_PENDING_REQUESTS => eprintln!(
                    "filepicker: refusing client {} — {MAX_PENDING_REQUESTS} connections are \
                     already waiting to say what they want",
                    conn.as_handle().0
                ),
                Ok(conn) => {
                    pending.push(Pending { conn, rx: RequestRx::new(), since: Instant::now() })
                }
            }
        }

        // `remove` rather than `swap_remove`: the entries after `i` shift down,
        // so leaving `i` alone visits each connection exactly once.
        let mut i = 0;
        while i < pending.len() {
            let handle = pending[i].conn.as_handle();
            if !ready.contains(&(TOKEN_PENDING_BASE + handle.0 as u64)) {
                i += 1;
                continue;
            }
            let step = {
                let p = &mut pending[i];
                p.rx.pump(&p.conn)
            };
            match step {
                RxStep::Idle => i += 1,
                // Unlogged: a caller may connect to find out whether it holds a
                // picker at all and hang up, which is its business.
                RxStep::Eof => {
                    pending.remove(i);
                }
                RxStep::Malformed => {
                    eprintln!(
                        "filepicker: dropping client {} — it sent a frame this protocol cannot \
                         describe",
                        handle.0
                    );
                    pending.remove(i);
                }
                RxStep::Frame { msg_type, payload_len } => {
                    let p = pending.remove(i);
                    if msg_type == MSG_FILEPICKER_REQUEST {
                        let data = p.rx.payload(payload_len);
                        let mode = if data.first() == Some(&(PickerMode::Save as u8)) {
                            PickerMode::Save
                        } else {
                            PickerMode::Open
                        };
                        let start_dir = data
                            .get(1..)
                            .and_then(|d| core::str::from_utf8(d).ok())
                            .filter(|s| !s.is_empty())
                            .unwrap_or("/");
                        run_picker(mode, start_dir, &p.conn);
                    }
                    // The session held the loop, so deadlines are re-read.
                    break;
                }
            }
        }
    }
}
