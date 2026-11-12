mod consent;

use filepicker_api::{
    PickerMode, MAX_REQUEST_BYTES, MSG_FILEPICKER_REQUEST, MSG_FILEPICKER_RESULT,
};
use font::Font;
use std::fs;
use std::time::Instant;
use toyos::admission::{self, Admission, Policy, Refused};
use toyos::poller::{Poller, READABLE};
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
const KEY_SPACE: u8 = 0x2C;

/// The desktop's monospace font, which the picker and the prompt draw at
/// their window's scale.
const FONT: &str = "/system/share/fonts/JetBrainsMono-Regular.ttf";

/// A window's text and spacing at its scale.
pub(crate) struct Ui {
    pub font: Font,
    scale: window::Scale,
}

impl Ui {
    pub fn at(ttf: &[u8], scale: window::Scale) -> Self {
        let (w, h) = scale.cell();
        let font = Font::from_ttf(ttf, w, h).unwrap_or_else(|e| panic!("filepicker: {FONT}: {e}"));
        Self { font, scale }
    }

    /// `window`'s scale's, if it is not this one's.
    pub fn follow(&mut self, ttf: &[u8], window: &Window) {
        if window.scale() != self.scale {
            *self = Self::at(ttf, window.scale());
        }
    }

    /// `l` logical pixels in the window's.
    pub fn px(&self, l: usize) -> usize {
        self.scale.px_u32(l as u32) as usize
    }
}

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
    font_h: usize,
    /// A bar's padding around its text.
    pad: usize,
}

impl Picker {
    fn new(mode: PickerMode, start_dir: &str, ui: &Ui) -> Self {
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
            font_h: ui.font.height(),
            pad: ui.px(8),
        }
    }

    /// Measured again for `ui`, a window's new scale's.
    fn measure(&mut self, ui: &Ui) {
        self.font_h = ui.font.height();
        self.pad = ui.px(8);
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

    /// A bar's height: a row of text and its padding.
    fn bar_h(&self) -> usize {
        self.font_h + self.pad
    }

    fn visible_rows(&self, win_h: usize) -> usize {
        let top = self.bar_h(); // path bar
        let bottom = if self.mode == PickerMode::Save {
            self.bar_h() * 2 // filename input + action bar
        } else {
            self.bar_h() // action bar only
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

fn render(fb: &Framebuffer, ui: &Ui, picker: &Picker) {
    let w = fb.width();
    let h = fb.height();
    let font = &ui.font;
    let fw = font.width();
    let fh = picker.font_h;
    let bar_h = picker.bar_h();
    let (pad, half, quarter) = (ui.px(8), ui.px(4), ui.px(2));

    fb.clear(BG);

    let path_str = picker.current_dir.to_string_lossy();
    fb.fill_rect(0, 0, w, bar_h, PATH_BG);
    font.draw_string(fb, pad, half, &path_str, TEXT_FG, PATH_BG);

    let list_y = bar_h;
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
        font.draw_string(fb, pad, y, &display, fg, bg);
    }

    let mut bottom_y = h;

    // Action bar (always at very bottom)
    bottom_y -= bar_h;
    let action_y = bottom_y;
    fb.fill_rect(0, action_y, w, bar_h, PATH_BG);

    let cancel_label = " Cancel ";
    let action_label = if picker.mode == PickerMode::Save {
        " Save "
    } else {
        " Open "
    };

    let action_w = action_label.len() * fw;
    let cancel_w = cancel_label.len() * fw;
    let action_x = w.saturating_sub(action_w + pad);
    let cancel_x = action_x.saturating_sub(cancel_w + pad);

    fb.fill_rect(cancel_x, action_y + quarter, cancel_w, fh + half, BUTTON_BG);
    font.draw_string(fb, cancel_x, action_y + half, cancel_label, BUTTON_FG, BUTTON_BG);

    fb.fill_rect(action_x, action_y + quarter, action_w, fh + half, ACCENT_BG);
    font.draw_string(fb, action_x, action_y + half, action_label, ACCENT_FG, ACCENT_BG);

    // Filename input (Save mode only)
    if picker.mode == PickerMode::Save {
        bottom_y -= bar_h;
        let input_y = bottom_y;
        fb.fill_rect(0, input_y, w, bar_h, PATH_BG);

        let label = "Filename: ";
        font.draw_string(fb, pad, input_y + half, label, DIM_FG, PATH_BG);

        let input_x = pad + label.len() * fw;
        let input_w = w.saturating_sub(input_x + pad);
        fb.fill_rect(input_x, input_y + quarter, input_w, fh + half, INPUT_BG);
        font.draw_string(fb, input_x + half, input_y + half, &picker.filename, TEXT_FG, INPUT_BG);

        if picker.focus_filename {
            let cx = input_x + half + picker.filename_cursor * fw;
            fb.fill_rect(cx, input_y + half, quarter, fh, CURSOR_COLOR);
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
            let list_y = picker.bar_h();
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
                let input_y = win_h.saturating_sub(picker.bar_h() * 2);
                if py >= input_y && py < input_y + picker.bar_h() {
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

fn run_picker(mode: PickerMode, start_dir: &str, client: &Connection, ttf: &[u8]) {
    let title = if mode == PickerMode::Save {
        "Save As"
    } else {
        "Open File"
    };

    // A refusal ends this session, not the daemon: the next request may well
    // arrive after whatever was holding the compositor's windows has exited.
    // The client is answered as if the user cancelled, because from its side
    // that is exactly what happened — no file was picked.
    let mut window = match Window::create_topmost(window::Size::Logical { width: 500, height: 400 }, title) {
        Ok(window) => window,
        Err(e) => {
            eprintln!("filepicker: {e}");
            let _ = client.send_bytes(MSG_FILEPICKER_RESULT, &[]);
            return;
        }
    };
    let mut fb = window.framebuffer();
    let mut ui = Ui::at(ttf, window.scale());
    let mut picker = Picker::new(mode, start_dir, &ui);

    render(&fb, &ui, &picker);
    window.present();

    loop {
        let event = window.recv_event();
        let mut needs_redraw = true;

        let result = match event {
            Event::Close => PickerResult::Cancel,

            Event::Resized => {
                fb = window.framebuffer();
                ui.follow(ttf, &window);
                picker.measure(&ui);
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
            render(&fb, &ui, &picker);
            window.present();
        }
    }
}

// --- Main daemon loop ---

/// Connections accepted and not yet asking, from either port: half are the
/// supervisor's questions' and half the picker's callers', so neither holds
/// the other out.
const ASKING: Policy = Policy { slots: 32, share: 16 };

/// Which port a connection came in on, which is whose share it spends.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Port {
    Picker,
    /// `consent`: the supervisor's question.
    Consent,
}

const TOKEN_ACCEPTOR: u64 = 0;
const TOKEN_CONSENT_ACCEPTOR: u64 = 1;
const TOKEN_ASKING: u64 = 2;

/// Serve `filepicker`, and the supervisor's questions on `consent`
/// ([`consent`]), for the rest of the machine's life.
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
    let questions = endow::acceptor(toyos_manifest::consent::PORT)
        .expect("the manifest declares this program serves `consent`");
    let ttf = fs::read(FONT).unwrap_or_else(|e| panic!("filepicker: {FONT}: {e}"));

    let poller = Poller::new(2 + ASKING.slots as u32);
    let mut asking: Admission<Port, (), Instant, MAX_REQUEST_BYTES> = Admission::new(ASKING);
    let mut ready: Vec<u64> = Vec::new();
    loop {
        if asking.has_room() {
            poller.watch(&acceptor, READABLE, TOKEN_ACCEPTOR);
            poller.watch(&questions, READABLE, TOKEN_CONSENT_ACCEPTOR);
        }
        asking.watch(|conn, slot| poller.watch(conn, READABLE, TOKEN_ASKING + slot));
        let timeout = asking.left(Instant::now()).map_or(u64::MAX, |left| left.as_nanos() as u64);
        ready.clear();
        poller.wait(1, timeout, |token| ready.push(token));

        for &token in &ready {
            let (acceptor, port) = match token {
                TOKEN_ACCEPTOR => (&acceptor, Port::Picker),
                TOKEN_CONSENT_ACCEPTOR => (&questions, Port::Consent),
                slot => {
                    asking.woke(slot - TOKEN_ASKING);
                    continue;
                }
            };
            // An accept that fails costs one connection and never the picker;
            // one past a full table stays queued until a slot is free.
            match asking.accept(acceptor) {
                None => {}
                Some(Err(e)) => eprintln!("filepicker: a connection could not be accepted ({e:?})"),
                Some(Ok(conn)) => {
                    if let Err(Refused { why, say: Some(unsaid), .. }) = asking.admit(conn, port, (), Instant::now()) {
                        eprintln!("filepicker: letting a connection go: {why}{unsaid}");
                    }
                }
            }
        }

        // The clock is read for each: a session is modal and holds the loop,
        // and a request that came whole meanwhile is served after it.
        while let Some(event) = asking.next(Instant::now()) {
            let asker = match event {
                admission::Event::Admitted(asker) => asker,
                admission::Event::Left { why, say: Some(unsaid), .. } => {
                    eprintln!("filepicker: letting a connection go: {why}{unsaid}");
                    continue;
                }
                admission::Event::Left { .. } => continue,
                admission::Event::Unsaid(unsaid) => {
                    eprintln!("filepicker: {}", unsaid.alone());
                    continue;
                }
            };
            match *asker.key() {
                Port::Consent => match consent::question(asker.payload(), asker.msg_type()) {
                    Ok(ask) => consent::ask(ask, &asker.into_connection(), &ttf),
                    Err(why) => eprintln!("filepicker: dropping the supervisor's question — {why}"),
                },
                Port::Picker if asker.msg_type() == MSG_FILEPICKER_REQUEST => {
                    let data = asker.payload();
                    let mode = if data.first() == Some(&(PickerMode::Save as u8)) {
                        PickerMode::Save
                    } else {
                        PickerMode::Open
                    };
                    let start_dir = data
                        .get(1..)
                        .and_then(|d| core::str::from_utf8(d).ok())
                        .filter(|s| !s.is_empty())
                        .unwrap_or("/")
                        .to_string();
                    run_picker(mode, &start_dir, &asker.into_connection(), &ttf);
                }
                Port::Picker => {}
            }
        }
    }
}
