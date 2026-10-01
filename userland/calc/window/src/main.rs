//! The window: a strip of keys under a display, laid out from whatever size the
//! surface happens to be.
//!
//! Snake's shape, for the same reason snake has it — one program that runs on
//! the development host and on ToyOS with nothing in it that knows the
//! difference. winit gives it a window and its events, softbuffer gives it a
//! wall of pixels, and everything the calculator actually decides lives in
//! `calc_core`, the package around this one.

use std::num::NonZeroU32;
use std::sync::Arc;

use calc_core::app::{enabled, Action, Button, Calc, Mode};
use calc_core::layout::{Fonts, Layout, Rect, Target, KEY_CHARS, MIN_H, MIN_W, OPEN_H, OPEN_W};
use calc_core::num::APPROX;
use calc_core::prog;
use font::{Color, Font};
use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, OwnedDisplayHandle};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

const BG: Color = Color { r: 0x1a, g: 0x1a, b: 0x2e };
const PANEL: Color = Color { r: 0x22, g: 0x22, b: 0x38 };
const SUNKEN: Color = Color { r: 0x18, g: 0x18, b: 0x2a };
const KEY_DIGIT: Color = Color { r: 0x2e, g: 0x2e, b: 0x48 };
const KEY_OP: Color = Color { r: 0x34, g: 0x34, b: 0x5c };
const KEY_FN: Color = Color { r: 0x28, g: 0x28, b: 0x40 };
const KEY_CLEAR: Color = Color { r: 0x4a, g: 0x2c, b: 0x38 };
const KEY_EQUALS: Color = Color { r: 0x2e, g: 0x6a, b: 0x3a };
const KEY_ACTIVE: Color = Color { r: 0x40, g: 0xb0, b: 0x40 };
const HOVER: Color = Color { r: 0x12, g: 0x12, b: 0x18 };
const PRESSED: Color = Color { r: 0x24, g: 0x24, b: 0x30 };
const TEXT: Color = Color { r: 0xe0, g: 0xe0, b: 0xe8 };
const DIM: Color = Color { r: 0x70, g: 0x70, b: 0x80 };
const OFF: Color = Color { r: 0x4a, g: 0x4a, b: 0x58 };
const ERROR: Color = Color { r: 0xe0, g: 0x50, b: 0x50 };

/// softbuffer's pixel is `0x00RRGGBB`.
const fn packed(c: Color) -> u32 {
    ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32
}

/// Lighten or darken a key, which is what hovering and pressing it look like.
fn shade(base: Color, by: Color, up: bool) -> Color {
    let mix = |a: u8, b: u8| if up { a.saturating_add(b) } else { a.saturating_sub(b) };
    Color { r: mix(base.r, by.r), g: mix(base.g, by.g), b: mix(base.b, by.b) }
}

/// The pixel buffer, as something that can be drawn on.
///
/// `font::Canvas::put_pixel` takes `&self`, so the buffer is reached through a
/// raw pointer — snake does the same, for the same reason.
struct Canvas {
    ptr: *mut u32,
    width: usize,
    height: usize,
}

impl Canvas {
    fn new(pixels: &mut [u32], width: usize, height: usize) -> Canvas {
        Canvas { ptr: pixels.as_mut_ptr(), width, height }
    }

    fn set(&self, x: i32, y: i32, color: Color) {
        if x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height {
            unsafe { *self.ptr.add(y as usize * self.width + x as usize) = packed(color) };
        }
    }

    fn fill(&self, r: Rect, color: Color) {
        for row in 0..r.h {
            for col in 0..r.w {
                self.set(r.x + col, r.y + row, color);
            }
        }
    }

    /// A one-pixel outline, which is how a pressed key says so.
    fn outline(&self, r: Rect, color: Color) {
        self.fill(Rect { h: 1, ..r }, color);
        self.fill(Rect { y: r.y + r.h - 1, h: 1, ..r }, color);
        self.fill(Rect { w: 1, ..r }, color);
        self.fill(Rect { x: r.x + r.w - 1, w: 1, ..r }, color);
    }

    fn text(&self, f: &Font, x: i32, y: i32, s: &str, fg: Color, bg: Color) {
        if x < 0 || y < 0 {
            return;
        }
        f.draw_string(self, x as usize, y as usize, s, fg, bg);
    }

    fn text_centred(&self, f: &Font, r: Rect, s: &str, fg: Color, bg: Color) {
        let chars = s.chars().count() as i32;
        let x = r.x + (r.w - chars * f.width() as i32) / 2;
        let y = r.y + (r.h - f.height() as i32) / 2;
        self.text(f, x, y, s, fg, bg);
    }
}

impl font::Canvas for Canvas {
    fn put_pixel(&self, x: usize, y: usize, color: Color) {
        self.set(x as i32, y as i32, color);
    }
}

struct App {
    context: Context<OwnedDisplayHandle>,
    ui: Option<Ui>,
}

struct Ui {
    window: Arc<Window>,
    surface: Surface<OwnedDisplayHandle, Arc<Window>>,
    fonts: Fonts,
    calc: Calc,
    width: u32,
    height: u32,
    hover: Option<Target>,
    pressed: Option<Target>,
    /// Where the pointer last was in the window: a button event says which
    /// button and not where.
    pointer: Option<(i32, i32)>,
}

impl Ui {
    fn new(elwt: &ActiveEventLoop, context: &Context<OwnedDisplayHandle>) -> Ui {
        let attrs = WindowAttributes::default()
            .with_title("Calculator")
            .with_inner_size(PhysicalSize::new(OPEN_W, OPEN_H))
            .with_min_inner_size(PhysicalSize::new(MIN_W as u32, MIN_H as u32));
        let window = Arc::new(elwt.create_window(attrs).unwrap());
        let size = window.inner_size();
        let mut surface = Surface::new(context, window.clone()).unwrap();
        let (w, h) = (size.width.max(1), size.height.max(1));
        surface.resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap()).unwrap();
        Ui {
            window,
            surface,
            fonts: Fonts::load(),
            calc: Calc::new(),
            width: w,
            height: h,
            hover: None,
            pressed: None,
            pointer: None,
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        let (w, h) = (width.max(1), height.max(1));
        self.width = w;
        self.height = h;
        self.surface.resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap()).unwrap();
    }

    fn layout(&self) -> Layout {
        Layout::new(self.width as i32, self.height as i32)
    }

    fn key(&mut self, key: &Key) {
        match key {
            Key::Named(NamedKey::Enter) => self.calc.act(Action::Equals),
            Key::Named(NamedKey::Escape) => self.calc.act(Action::Clear),
            Key::Named(NamedKey::Backspace) => self.calc.act(Action::Backspace),
            Key::Named(NamedKey::Delete) => self.calc.act(Action::Delete),
            Key::Named(NamedKey::ArrowLeft) => self.calc.act(Action::Left),
            Key::Named(NamedKey::ArrowRight) => self.calc.act(Action::Right),
            Key::Named(NamedKey::Home) => self.calc.act(Action::Home),
            Key::Named(NamedKey::End) => self.calc.act(Action::End),
            Key::Named(NamedKey::Tab) => {
                let other = match self.calc.mode() {
                    Mode::Calc => Mode::Prog,
                    Mode::Prog => Mode::Calc,
                };
                self.calc.act(Action::SetMode(other));
            }
            Key::Character(s) => {
                for c in s.chars() {
                    if c == '=' {
                        self.calc.act(Action::Equals);
                    } else {
                        self.calc.type_char(c);
                    }
                }
            }
            _ => {}
        }
    }

    fn redraw(&mut self) {
        let layout = self.layout();
        let (w, h) = (self.width as usize, self.height as usize);
        // The scene borrows the fields the drawing reads; the buffer borrows
        // the surface. Two disjoint halves of one `Ui`.
        let scene = Scene {
            calc: &self.calc,
            fonts: &self.fonts,
            layout: &layout,
            hover: self.hover,
            pressed: self.pressed,
        };
        let mut buffer = self.surface.buffer_mut().unwrap();
        let pixels: &mut [u32] = &mut buffer;
        let canvas = Canvas::new(pixels, w, h);
        canvas.fill(Rect { x: 0, y: 0, w: w as i32, h: h as i32 }, BG);

        scene.draw_tabs(&canvas);
        scene.draw_display(&canvas);
        scene.draw_message(&canvas);
        scene.draw_keys(&canvas);

        buffer.present().unwrap();
    }
}

/// Everything one frame is drawn from, and nothing that can change while it is.
struct Scene<'a> {
    calc: &'a Calc,
    fonts: &'a Fonts,
    layout: &'a Layout,
    hover: Option<Target>,
    pressed: Option<Target>,
}

impl Scene<'_> {
    fn draw_tabs(&self, canvas: &Canvas) {
        for (i, mode) in [Mode::Calc, Mode::Prog].into_iter().enumerate() {
            let rect = self.layout.tabs[i];
            let active = self.calc.mode() == mode;
            let mut base = if active { KEY_EQUALS } else { KEY_FN };
            if self.hover == Some(Target::Tab(mode)) {
                base = shade(base, HOVER, true);
            }
            if self.pressed == Some(Target::Tab(mode)) {
                base = shade(base, PRESSED, false);
            }
            canvas.fill(rect, base);
            if active {
                canvas.outline(rect, KEY_ACTIVE);
            }
            let label = match mode {
                Mode::Calc => "Calc",
                Mode::Prog => "Prog",
            };
            let f = self.fonts.fitting(label.len() as i32, rect.w - 8, rect.h - 8);
            canvas.text_centred(f, rect, label, if active { TEXT } else { DIM }, base);
        }

        // What the layout is standing on, right-aligned in the same strip.
        let note = match self.calc.mode() {
            Mode::Calc => self.calc.angle_label(),
            Mode::Prog => self.calc.base().label(),
        };
        let strip = self.layout.strip;
        let f = self.fonts.fitting(KEY_CHARS, strip.w / 4, strip.h - 8);
        let x = strip.x + strip.w - note.chars().count() as i32 * f.width() as i32;
        canvas.text(f, x, strip.y + (strip.h - f.height() as i32) / 2, note, DIM, BG);
    }

    fn draw_display(&self, canvas: &Canvas) {
        canvas.fill(self.layout.display, SUNKEN);
        let inner = self.layout.display.inset(self.layout.pad);
        match self.calc.mode() {
            Mode::Calc => self.draw_calc_display(canvas, inner),
            Mode::Prog => self.draw_prog_display(canvas, inner),
        }
    }

    /// The entry line, scrolled so the caret is always on it, and the caret.
    /// Returns the height it took.
    fn draw_entry(&self, canvas: &Canvas, inner: Rect, f: &Font) -> i32 {
        let expr = self.calc.expr();
        let per = (inner.w / f.width() as i32).max(1) as usize;
        let caret_at = expr[..self.calc.caret()].chars().count();
        let scroll = caret_at.saturating_sub(per.saturating_sub(1));
        let shown: String = expr.chars().skip(scroll).take(per).collect();
        canvas.text(f, inner.x, inner.y, &shown, TEXT, SUNKEN);
        let caret_x = inner.x + (caret_at - scroll) as i32 * f.width() as i32;
        canvas.fill(
            Rect { x: caret_x, y: inner.y - 2, w: 2, h: f.height() as i32 + 4 },
            TEXT,
        );
        f.height() as i32
    }

    fn draw_calc_display(&self, canvas: &Canvas, inner: Rect) {
        let (f, _) = self.fonts.fit(self.calc.expr(), inner.w, 1);
        self.draw_entry(canvas, inner, f);

        // The result, as large as it fits and wrapped rather than cut.
        let Some(text) = self.calc.preview() else { return };
        let (rf, lines) = self.fonts.fit(&text, inner.w, 2);
        let colour = if text.starts_with(APPROX) { DIM } else { TEXT };
        let top = inner.y + inner.h - lines.len() as i32 * rf.height() as i32;
        for (i, line) in lines.iter().enumerate() {
            let width = line.chars().count() as i32 * rf.width() as i32;
            canvas.text(
                rf,
                inner.x + inner.w - width,
                top + i as i32 * rf.height() as i32,
                line,
                colour,
                SUNKEN,
            );
        }
    }

    fn draw_prog_display(&self, canvas: &Canvas, inner: Rect) {
        // Four rows of panes under the entry line, all in one cell size: the
        // binary pane is 35 characters and the widest thing here, so it decides.
        let f = self.fonts.fitting(40, inner.w, inner.h / 6);
        let cell = f.height() as i32;
        self.draw_entry(canvas, inner, f);

        let value = self.calc.value();
        let (high, low) = prog::pane_bin(value);
        let rows: [(&str, &str); 4] = [
            ("HEX", &prog::pane_hex(value)),
            ("DEC", &prog::pane_dec(value)),
            ("BIN", &high),
            ("", &low),
        ];
        let label_w = 4 * f.width() as i32;
        let step = ((inner.h - cell) / rows.len() as i32).max(cell);
        for (i, (label, text)) in rows.iter().enumerate() {
            let y = inner.y + cell + 4 + i as i32 * step;
            let active = self.calc.base().label() == *label;
            canvas.text(f, inner.x, y, label, if active { TEXT } else { DIM }, SUNKEN);
            let width = text.chars().count() as i32 * f.width() as i32;
            canvas.text(f, inner.x + (inner.w - width).max(label_w), y, text, TEXT, SUNKEN);
        }
    }

    fn draw_message(&self, canvas: &Canvas) {
        let Some(message) = self.calc.message() else { return };
        let r = self.layout.message;
        let (f, lines) = self.fonts.fit(message, r.w, 1);
        canvas.text(f, r.x, r.y + (r.h - f.height() as i32) / 2, &lines[0], ERROR, BG);
    }

    fn draw_keys(&self, canvas: &Canvas) {
        canvas.fill(self.layout.panel, PANEL);
        let first = self.layout.keys[0];
        let f = self.fonts.fitting(KEY_CHARS, first.w - 8, first.h - 8);
        for (i, button) in self.calc.buttons().iter().enumerate() {
            let rect = self.layout.keys[i];
            let live = enabled(button, self.calc.mode(), self.calc.base());
            let on = is_on(button, self.calc);
            let mut colour = if on { KEY_EQUALS } else { key_colour(button) };
            if !live {
                colour = KEY_FN;
            } else if self.hover == Some(Target::Key(i)) {
                colour = shade(colour, HOVER, true);
            }
            if self.pressed == Some(Target::Key(i)) && live {
                colour = shade(colour, PRESSED, false);
            }
            canvas.fill(rect, colour);
            if self.pressed == Some(Target::Key(i)) && live {
                canvas.outline(rect, KEY_ACTIVE);
            }
            let label = match button.action {
                Action::ToggleAngle => self.calc.angle_label(),
                _ => button.label,
            };
            canvas.text_centred(f, rect, label, if live { TEXT } else { OFF }, colour);
        }
    }
}

/// Whether this button shows the state it selects, rather than an action.
fn is_on(button: &Button, calc: &Calc) -> bool {
    match button.action {
        Action::SetBase(base) => calc.mode() == Mode::Prog && calc.base() == base,
        _ => false,
    }
}

fn key_colour(button: &Button) -> Color {
    match button.action {
        Action::Equals => KEY_EQUALS,
        Action::Clear | Action::Backspace => KEY_CLEAR,
        Action::SetBase(_) | Action::ToggleAngle => KEY_FN,
        // A value looks like a value whether it is a digit or a constant, and a
        // function looks like a function whether it is spelled or drawn.
        Action::Insert("π") => KEY_DIGIT,
        Action::Insert("√") => KEY_FN,
        Action::Insert(text) if text.ends_with('(') => KEY_FN,
        Action::Insert(text) => {
            let value = text.chars().count() == 1
                && text.chars().next().is_some_and(|c| c.is_ascii_alphanumeric() || c == '.');
            if value {
                KEY_DIGIT
            } else {
                KEY_OP
            }
        }
        _ => KEY_OP,
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.ui.is_none() {
            self.ui = Some(Ui::new(event_loop, &self.context));
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(ui) = self.ui.as_mut() else { return };
        let mut dirty = false;
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
                return;
            }
            WindowEvent::Resized(size) => {
                ui.resize(size.width, size.height);
                // Whatever the pointer was over is somewhere else now.
                ui.hover = None;
                ui.pressed = None;
                dirty = true;
            }
            WindowEvent::RedrawRequested => {
                ui.redraw();
                return;
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    ui.key(&event.logical_key);
                    dirty = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                ui.pointer = Some((position.x as i32, position.y as i32));
                let over = ui.layout().hit(position.x as i32, position.y as i32);
                if over != ui.hover {
                    ui.hover = over;
                    dirty = true;
                }
            }
            WindowEvent::CursorLeft { .. } => {
                ui.pointer = None;
                if ui.hover.is_some() || ui.pressed.is_some() {
                    ui.hover = None;
                    ui.pressed = None;
                    dirty = true;
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button != MouseButton::Left {
                    return;
                }
                let over = ui.pointer.and_then(|(x, y)| ui.layout().hit(x, y));
                ui.hover = over;
                match state {
                    ElementState::Pressed => ui.pressed = over,
                    ElementState::Released => {
                        // A press only counts where it started and ended on the
                        // same key.
                        if let (Some(down), Some(up)) = (ui.pressed, over) {
                            if down == up {
                                match up {
                                    Target::Tab(mode) => ui.calc.act(Action::SetMode(mode)),
                                    Target::Key(i) => {
                                        let button = &ui.calc.buttons()[i];
                                        if enabled(button, ui.calc.mode(), ui.calc.base()) {
                                            let action = button.action;
                                            ui.calc.act(action);
                                        }
                                    }
                                }
                            }
                        }
                        ui.pressed = None;
                    }
                }
                dirty = true;
            }
            _ => {}
        }
        if dirty {
            ui.window.request_redraw();
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    let context = Context::new(event_loop.owned_display_handle()).unwrap();
    event_loop.run_app(&mut App { context, ui: None }).unwrap();
}
