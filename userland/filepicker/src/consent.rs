//! The question `/system/bin/supervisor` asks before an installed package's
//! launch, drawn in the compositor's prompt layer ([`toyos_manifest::consent`]).
//!
//! **A chooser bounded by the folder rules**: it starts at the session user's
//! home and lists only folders a package may be granted
//! ([`grants::folder`]), never `Apps`, a link, or a folder past a grant's
//! bounds, so nothing the person can pick is refused for its shape. The
//! supervisor holds the answer to the same rules regardless.
//!
//! **Nothing answers but the person, and only once they could read it**: the
//! compositor gives the prompt only what was pressed after its first frame was
//! on the panel, and a button answers only on a release over the button its
//! press began on. The keyboard starts on the folder list, where Enter opens a
//! folder and answers nothing; an answer is a button reached with Tab. Escape
//! and the window closing are Skip, and the supervisor closing the connection
//! takes the question down unanswered.

use std::fs;
use std::path::{Path, PathBuf};

use font::Font;
use toyos::ipc::{self, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::Connection;
use toyos_manifest::consent::{self, Ask, Reply};
use toyos_manifest::grants::{self, Access, Folder};
use window::{Color, Event, Framebuffer, KeyPress, MouseEvent, Window};

use crate::{
    ACCENT_BG, ACCENT_FG, BG, BUTTON_BG, BUTTON_FG, DIM_FG, DIR_FG, KEY_BACKSPACE, KEY_DOWN, KEY_ENTER, KEY_ESCAPE,
    KEY_LEFT, KEY_RIGHT, KEY_SPACE, KEY_TAB, KEY_UP, PATH_BG, SEL_BG, TEXT_FG,
};

const WIDTH: u32 = 600;
const HEIGHT: u32 = 400;

/// What has the keyboard, in Tab's order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    List,
    ReadOnly,
    Button(Choice),
}

/// The three answers a button gives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Choice {
    Once,
    Always,
    Deny,
}

const CHOICES: [(Choice, &str); 3] =
    [(Choice::Once, " Allow once "), (Choice::Always, " Always allow "), (Choice::Deny, " Deny ")];

/// One question being asked.
struct Question {
    ask: Ask,
    home: PathBuf,
    dir: PathBuf,
    /// The folders of `dir` a package may be granted, sorted.
    folders: Vec<String>,
    selected: usize,
    scroll: usize,
    focus: Focus,
    /// The person grants less than was asked: open, and not change.
    read_only: bool,
    /// What a press of the left button began on, which only a release over it
    /// answers.
    pressed: Option<Focus>,
}

impl Question {
    fn new(ask: Ask) -> Self {
        let home = PathBuf::from(toyos_manifest::session_home());
        let mut question = Self {
            ask,
            dir: home.clone(),
            home,
            folders: Vec::new(),
            selected: 0,
            scroll: 0,
            focus: Focus::List,
            read_only: false,
            pressed: None,
        };
        question.list();
        question
    }

    fn list(&mut self) {
        self.folders = grantable(&self.dir);
        self.selected = 0;
        self.scroll = 0;
    }

    /// The folder an Allow grants: the one selected.
    fn chosen(&self) -> Option<Folder> {
        let name = self.folders.get(self.selected)?;
        let path = self.dir.join(name).to_str()?.to_string();
        let access = if self.read_only { Access::ReadOnly } else { self.ask.access };
        Some(Folder { path, access })
    }

    /// Tab's stops, in order: the read-only box only where the package asked
    /// to change its folder.
    fn stops(&self) -> Vec<Focus> {
        let mut stops = vec![Focus::List];
        if self.ask.access == Access::ReadWrite {
            stops.push(Focus::ReadOnly);
        }
        stops.extend(CHOICES.iter().map(|(choice, _)| Focus::Button(*choice)));
        stops
    }

    fn tab(&mut self, back: bool) {
        let stops = self.stops();
        let at = stops.iter().position(|s| *s == self.focus).unwrap_or(0);
        let next = if back { (at + stops.len() - 1) % stops.len() } else { (at + 1) % stops.len() };
        self.focus = stops[next];
    }

    /// The answer `focus` gives when it is activated, or `None` where it only
    /// moves the chooser.
    fn activate(&mut self, focus: Focus) -> Option<Reply> {
        match focus {
            Focus::List => {
                self.enter();
                None
            }
            Focus::ReadOnly => {
                self.read_only = !self.read_only;
                None
            }
            Focus::Button(Choice::Deny) => Some(Reply::Deny),
            Focus::Button(Choice::Once) => self.chosen().map(Reply::Once),
            Focus::Button(Choice::Always) => self.chosen().map(Reply::Always),
        }
    }

    /// Into the selected folder.
    fn enter(&mut self) {
        if let Some(name) = self.folders.get(self.selected) {
            self.dir.push(name);
            self.list();
        }
    }

    /// Back toward the home, never above it.
    fn up(&mut self) {
        if self.dir != self.home {
            self.dir.pop();
            self.list();
        }
    }

    fn key(&mut self, key: &KeyPress) -> Option<Reply> {
        if !key.pressed() {
            return None;
        }
        match key.keycode {
            KEY_ESCAPE => return Some(Reply::Skip),
            KEY_TAB => self.tab(key.shift()),
            KEY_ENTER => return self.activate(self.focus),
            KEY_SPACE if self.focus != Focus::List => return self.activate(self.focus),
            KEY_UP if self.focus == Focus::List => {
                self.selected = self.selected.saturating_sub(1);
                self.scroll = self.scroll.min(self.selected);
            }
            KEY_DOWN if self.focus == Focus::List => {
                if self.selected + 1 < self.folders.len() {
                    self.selected += 1;
                }
                if self.selected >= self.scroll + ROWS {
                    self.scroll = self.selected + 1 - ROWS;
                }
            }
            KEY_RIGHT if self.focus == Focus::List => self.enter(),
            KEY_LEFT | KEY_BACKSPACE if self.focus == Focus::List => self.up(),
            _ => {}
        }
        None
    }

    fn mouse(&mut self, mouse: &MouseEvent, font: &Font) -> Option<Reply> {
        let at = target(self, font, mouse.x as usize, mouse.y as usize);
        match mouse.event_type {
            window::MOUSE_PRESS if mouse.changed == 1 => {
                self.pressed = at;
                if let Some(Focus::List) = at {
                    if let Some(row) = row_at(font, mouse.y as usize) {
                        if self.scroll + row < self.folders.len() {
                            self.selected = self.scroll + row;
                        }
                    }
                }
                if let Some(focus) = at {
                    self.focus = focus;
                }
                None
            }
            window::MOUSE_RELEASE if mouse.changed == 1 => match self.pressed.take() {
                Some(pressed) if Some(pressed) == at && pressed != Focus::List => self.activate(pressed),
                _ => None,
            },
            _ => None,
        }
    }
}

/// The folders of `dir` a package may be granted: directories, not links,
/// each one [`grants::folder`] admits.
fn grantable(dir: &Path) -> Vec<String> {
    let mut folders: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|name| dir.join(name).to_str().is_some_and(|path| grants::folder(path).is_ok()))
        .collect();
    folders.sort_by_key(|name| name.to_lowercase());
    folders
}

/// The rows the folder list shows at once.
const ROWS: usize = 12;

/// Where the folder list's first row is.
fn list_y(font: &Font) -> usize {
    (font.height() + 8) * 3
}

fn row_at(font: &Font, y: usize) -> Option<usize> {
    let top = list_y(font);
    (y >= top && y < top + ROWS * font.height()).then(|| (y - top) / font.height())
}

/// The read-only box's row.
fn toggle_y(font: &Font) -> usize {
    list_y(font) + ROWS * font.height() + 8
}

fn buttons_y() -> usize {
    HEIGHT as usize - 36
}

/// Each button's left edge and width, right to left from the window's edge.
fn button_rects(font: &Font) -> [(Choice, usize, usize); 3] {
    let mut x = WIDTH as usize - 8;
    let mut out = [(Choice::Deny, 0, 0); 3];
    for (i, (choice, label)) in CHOICES.iter().enumerate().rev() {
        let w = label.len() * font.width();
        x -= w;
        out[i] = (*choice, x, w);
        x -= 8;
    }
    out
}

/// What is at `(x, y)`.
fn target(question: &Question, font: &Font, x: usize, y: usize) -> Option<Focus> {
    if row_at(font, y).is_some() {
        return Some(Focus::List);
    }
    let toggle = toggle_y(font);
    if question.ask.access == Access::ReadWrite && (toggle..toggle + font.height()).contains(&y) {
        return Some(Focus::ReadOnly);
    }
    let top = buttons_y();
    if !(top..top + font.height() + 8).contains(&y) {
        return None;
    }
    button_rects(font)
        .into_iter()
        .find(|(_, left, w)| (*left..left + w).contains(&x))
        .map(|(choice, _, _)| Focus::Button(choice))
}

fn render(fb: &Framebuffer, font: &Font, question: &Question) {
    let (w, fh) = (fb.width(), font.height());
    fb.clear(BG);
    font.draw_string(fb, 8, 8, &question.ask.words(), TEXT_FG, BG);
    let shown = question.dir.to_string_lossy();
    fb.fill_rect(0, fh + 16, w, fh + 8, PATH_BG);
    font.draw_string(fb, 8, fh + 20, &format!("Choose the folder, in {shown}"), TEXT_FG, PATH_BG);

    let top = list_y(font);
    if question.folders.is_empty() {
        font.draw_string(fb, 8, top, "  (no folder here can be given; Left goes back)", DIM_FG, BG);
    }
    for (row, name) in question.folders.iter().enumerate().skip(question.scroll).take(ROWS) {
        let y = top + (row - question.scroll) * fh;
        let selected = row == question.selected;
        let bg = match (selected, question.focus == Focus::List) {
            (true, true) => SEL_BG,
            (true, false) => PATH_BG,
            (false, _) => BG,
        };
        fb.fill_rect(0, y, w, fh, bg);
        font.draw_string(fb, 8, y, &format!("  {name}/"), DIR_FG, bg);
    }

    if question.ask.access == Access::ReadWrite {
        let y = toggle_y(font);
        let bg = if question.focus == Focus::ReadOnly { SEL_BG } else { BG };
        let mark = if question.read_only { "[x]" } else { "[ ]" };
        let line = format!("{mark} Only let it open the files, not change them");
        fb.fill_rect(0, y, w, fh, bg);
        font.draw_string(fb, 8, y, &line, TEXT_FG, bg);
    }

    let y = buttons_y();
    fb.fill_rect(0, y - 4, w, fh + 16, PATH_BG);
    font.draw_string(fb, 8, y + 4, "Esc: not now", DIM_FG, PATH_BG);
    for (choice, left, width) in button_rects(font) {
        let label = CHOICES.iter().find(|(c, _)| *c == choice).map_or("", |(_, l)| l);
        let (bg, fg): (Color, Color) = match question.focus == Focus::Button(choice) {
            true => (ACCENT_BG, ACCENT_FG),
            false => (BUTTON_BG, BUTTON_FG),
        };
        fb.fill_rect(left, y, width, fh + 8, bg);
        font.draw_string(fb, left, y + 4, label, fg, bg);
    }
}

/// Ask `ask` of the person at the screen and answer the supervisor on `conn`,
/// unless the supervisor withdraws it first.
pub fn ask(ask: Ask, conn: &Connection, font: &Font) {
    let package = ask.package.clone();
    let mut window = match Window::create_on(consent::PROMPT, WIDTH, HEIGHT, "") {
        Ok(window) => window,
        Err(e) => {
            // The supervisor starts the package with no folder and asks again
            // at its next launch.
            eprintln!("filepicker: no prompt for {package}'s question ({e}); it is skipped");
            reply(conn, &package, Reply::Skip);
            return;
        }
    };
    let mut fb = window.framebuffer();
    let mut question = Question::new(ask);
    render(&fb, font, &question);
    window.present();
    println!("filepicker: asking about {package}");

    const WINDOW: u64 = 0;
    const SUPERVISOR: u64 = 1;
    let poller = Poller::new(2);
    let mut withdrawn = ipc::FrameRx::<0>::new();
    loop {
        poller.watch_raw(window.handle(), READABLE, WINDOW);
        poller.watch(conn, READABLE, SUPERVISOR);
        let (mut supervisor, mut shown) = (false, false);
        poller.wait(1, u64::MAX, |token| match token {
            SUPERVISOR => supervisor = true,
            _ => shown = true,
        });
        // Anything on the supervisor's connection after its question is the
        // question taken back.
        if supervisor && !matches!(withdrawn.pump(conn), RxStep::Idle) {
            println!("filepicker: the question about {package} was withdrawn");
            return;
        }
        // One event per wake, read off the readiness this poller saw: a second
        // poller on the window's handle (`Window::poll_event`) would take the
        // next readiness from this one. A handle with more to read is ready
        // again at the next watch.
        if !shown {
            continue;
        }
        let answer = match window.recv_event() {
            Event::Close => Some(Reply::Skip),
            Event::Resized => {
                fb = window.framebuffer();
                None
            }
            Event::KeyInput(key) => question.key(&window.press(key)),
            Event::MouseInput(mouse) => question.mouse(&mouse, font),
            _ => continue,
        };
        if let Some(answer) = answer {
            reply(conn, &package, answer);
            return;
        }
        render(&fb, font, &question);
        window.present();
    }
}

fn reply(conn: &Connection, package: &str, answer: Reply) {
    if let Err(e) = conn.try_send_bytes(consent::MSG_REPLY, &answer.encode()) {
        eprintln!("filepicker: the answer about {package} did not reach the supervisor ({e:?})");
    }
}

/// The first frame on a connection off `consent`, read whole: the question,
/// or why it is none.
pub fn question(payload: &[u8], msg_type: u32) -> Result<Ask, String> {
    if msg_type != consent::MSG_ASK {
        return Err(format!("message {msg_type} is no question"));
    }
    if payload.len() > consent::MAX_ASK {
        return Err(format!("it is longer than the {} bytes any question is", consent::MAX_ASK));
    }
    Ask::decode(payload).ok_or_else(|| "it is not one the protocol has".to_string())
}
