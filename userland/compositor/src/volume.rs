//! The volume keys and the level overlay.
//!
//! soundserver owns the level and mute; the desktop only asks it to change and
//! draws the answer it gets back, never a level of its own guessing.

use std::time::{Duration, Instant};

use toyos::audio::{MasterAdjust, MasterState, MSG_MASTER_ADJUST, MSG_MASTER_STATE};
use toyos::ipc::{self, RxStep};
use toyos::{endow, AsHandle, Connection};
use toyos_desktop::{Rect, VolumeKey};
use window::{Color, Framebuffer};

/// Percentage points one press moves the level.
const STEP: i32 = 5;
/// How long the overlay stays whole after the last change.
const HOLD: Duration = Duration::from_millis(1500);
/// How long it then takes to fade out.
const FADE: Duration = Duration::from_millis(300);

const W: i32 = 300;
const H: i32 = 56;
const RADIUS: i32 = 14;
/// Above the taskbar by this much.
const LIFT: i32 = 40;
/// The overlay's opacity at full strength, out of 255.
const OPACITY: u32 = 235;

const BG: Color = Color { r: 0x20, g: 0x20, b: 0x30 };
const BORDER: Color = Color { r: 0x58, g: 0x58, b: 0x6e };
const TRACK: Color = Color { r: 0x38, g: 0x38, b: 0x42 };
const FILL: Color = Color { r: 0xe0, g: 0xe0, b: 0xe8 };
const FILL_MUTED: Color = Color { r: 0x60, g: 0x60, b: 0x70 };
const TEXT: Color = Color { r: 0xe0, g: 0xe0, b: 0xe8 };

type StateRx = ipc::FrameRx<{ core::mem::size_of::<MasterState>() + 1 }>;

pub struct Volume {
    conn: Option<Connection>,
    rx: StateRx,
    /// The overlay's pixels, in the screen's format, redrawn on each answer.
    _pixels: Vec<u8>,
    canvas: Framebuffer,
    shown_at: Option<Instant>,
    /// Drawn since the last frame took its damage.
    fresh: bool,
    /// Where it was last drawn, so the frame it vanishes on repaints there.
    rect: Rect,
}

impl Volume {
    pub fn new(pixel_format: u32) -> Self {
        let mut pixels = vec![0u8; (W * H * 4) as usize];
        let canvas = Framebuffer::new(pixels.as_mut_ptr(), W as usize, H as usize, W as usize, pixel_format);
        Self { conn: None, rx: StateRx::new(), _pixels: pixels, canvas, shown_at: None, fresh: false, rect: Rect::new(0, 0, 0, 0) }
    }

    /// The connection's handle, for the poller; `None` until the first key.
    pub fn handle(&self) -> Option<toyos_abi::RawHandle> {
        self.conn.as_ref().map(|c| c.as_handle())
    }

    /// Ask soundserver to move the level; returns a connection that is new and
    /// so needs watching.
    pub fn key(&mut self, key: VolumeKey) -> bool {
        println!("compositor: volume key {key:?}");
        let mut fresh = false;
        if self.conn.is_none() {
            match endow::service("soundserver") {
                Ok(conn) => {
                    self.conn = Some(conn);
                    self.rx = StateRx::new();
                    fresh = true;
                }
                Err(e) => {
                    println!("compositor: no soundserver for the volume keys ({e:?})");
                    return false;
                }
            }
        }
        let req = match key {
            VolumeKey::Mute => MasterAdjust { step: 0, toggle_mute: 1 },
            VolumeKey::Up => MasterAdjust { step: STEP, toggle_mute: 0 },
            VolumeKey::Down => MasterAdjust { step: -STEP, toggle_mute: 0 },
        };
        let conn = self.conn.as_ref().expect("connected above");
        if let Err(e) = conn.try_send(MSG_MASTER_ADJUST, &req) {
            println!("compositor: soundserver did not take a volume request ({e:?}); reconnecting on the next key");
            self.conn = None;
            return false;
        }
        fresh
    }

    /// Read every answer soundserver sent; `true` while the connection lives
    /// and wants watching again.
    pub fn drain(&mut self, font: &font::Font, screen: Rect, taskbar_h: i32) -> bool {
        let Some(conn) = self.conn.as_ref() else { return false };
        let mut latest = None;
        loop {
            match self.rx.pump(conn) {
                RxStep::Idle => break,
                RxStep::Frame { msg_type: MSG_MASTER_STATE, payload_len }
                    if payload_len == core::mem::size_of::<MasterState>() =>
                {
                    latest = ipc::decode_payload::<MasterState>(self.rx.payload(payload_len)).ok();
                }
                other => {
                    println!("compositor: soundserver's volume connection ended ({other:?})");
                    self.conn = None;
                    return false;
                }
            }
        }
        if let Some(state) = latest {
            println!("compositor: volume {}%{}", state.percent, if state.muted != 0 { " muted" } else { "" });
            self.draw(font, state);
            self.rect = Rect::new((screen.w() - W) / 2, screen.h() - taskbar_h - LIFT - H, W, H);
            self.shown_at = Some(Instant::now());
            self.fresh = true;
        }
        true
    }

    /// Where the overlay is and how opaque, out of 255; `None` once it has faded.
    fn alpha(&self, now: Instant) -> Option<u32> {
        let age = now.duration_since(self.shown_at?);
        if age < HOLD {
            return Some(OPACITY);
        }
        let into = age - HOLD;
        if into >= FADE {
            return None;
        }
        Some(OPACITY * (FADE - into).as_millis() as u32 / FADE.as_millis() as u32)
    }

    /// The rect a frame must repaint: all of it while the overlay is showing
    /// or fading, and once more on the frame it is gone.
    pub fn damage(&mut self, now: Instant) -> Option<Rect> {
        let shown = self.shown_at?;
        let age = now.duration_since(shown);
        if age >= HOLD + FADE {
            self.shown_at = None;
            return Some(self.rect);
        }
        if age < HOLD && !std::mem::take(&mut self.fresh) {
            return None;
        }
        Some(self.rect)
    }

    /// Blend the overlay over `region` of `back`, which was just composed.
    pub fn paint(&self, back: &Framebuffer, region: Rect, now: Instant) {
        let Some(alpha) = self.alpha(now) else { return };
        let clip = region.intersect(self.rect);
        if clip.is_empty() {
            return;
        }
        for y in clip.y0..clip.y1 {
            let oy = y - self.rect.y0;
            for x in clip.x0..clip.x1 {
                let ox = x - self.rect.x0;
                let cover = corner_cover(ox, oy);
                if cover == 0 {
                    continue;
                }
                let a = alpha * cover / 255;
                let src = self.canvas.get_pixel(ox as usize, oy as usize);
                let dst = back.get_pixel(x as usize, y as usize);
                let mix = |s: u8, d: u8| ((s as u32 * a + d as u32 * (255 - a)) / 255) as u8;
                back.put_pixel(x as usize, y as usize, Color { r: mix(src.r, dst.r), g: mix(src.g, dst.g), b: mix(src.b, dst.b) });
            }
        }
    }

    fn draw(&mut self, font: &font::Font, state: MasterState) {
        let c = &self.canvas;
        let muted = state.muted != 0;
        c.fill_rect(0, 0, W as usize, H as usize, BG);
        // A one-pixel border just inside the rounded edge.
        for y in 0..H {
            for x in 0..W {
                let inner = corner_cover_inset(x, y, 1);
                if corner_cover(x, y) > 0 && inner < 255 {
                    c.put_pixel(x as usize, y as usize, BORDER);
                }
            }
        }

        let icon_x = 18;
        let cy = H / 2;
        draw_speaker(c, icon_x, cy, if muted { FILL_MUTED } else { FILL }, muted, state.percent);

        let text = if muted { String::from("muted") } else { format!("{}%", state.percent) };
        let text_w = (text.len() * font.width()) as i32;
        let text_x = W - 18 - 6 * font.width() as i32 + (6 * font.width() as i32 - text_w);
        font.draw_string(c, text_x as usize, (cy - 8) as usize, &text, TEXT, BG);

        let bar_x0 = icon_x + 40;
        let bar_x1 = W - 18 - 6 * font.width() as i32 - 12;
        let bar_h = 6;
        let bar_y = cy - bar_h / 2;
        fill_round(c, bar_x0, bar_y, bar_x1 - bar_x0, bar_h, TRACK);
        let filled = (bar_x1 - bar_x0) * state.percent as i32 / 100;
        if filled > 0 {
            fill_round(c, bar_x0, bar_y, filled, bar_h, if muted { FILL_MUTED } else { FILL });
        }
    }
}

/// How much of the overlay's pixel `(x, y)` lies inside its rounded outline, out of 255.
fn corner_cover(x: i32, y: i32) -> u32 {
    corner_cover_inset(x, y, 0)
}

fn corner_cover_inset(x: i32, y: i32, inset: i32) -> u32 {
    let r = RADIUS - inset;
    let (x0, y0, x1, y1) = (inset, inset, W - inset, H - inset);
    if x < x0 || y < y0 || x >= x1 || y >= y1 {
        return 0;
    }
    let cx = if x < x0 + r { x0 + r } else if x >= x1 - r { x1 - r } else { return 255 };
    let cy = if y < y0 + r { y0 + r } else if y >= y1 - r { y1 - r } else { return 255 };
    // Four samples per pixel are enough to soften the edge.
    let mut inside = 0;
    for (sx, sy) in [(0.25f32, 0.25f32), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
        let dx = x as f32 + sx - cx as f32;
        let dy = y as f32 + sy - cy as f32;
        if dx * dx + dy * dy <= (r * r) as f32 {
            inside += 1;
        }
    }
    inside * 255 / 4
}

/// A bar with round ends, drawn opaque onto the overlay's own background.
fn fill_round(c: &Framebuffer, x: i32, y: i32, w: i32, h: i32, color: Color) {
    let r = h as f32 / 2.0;
    for py in 0..h {
        for px in 0..w {
            let fx = px as f32 + 0.5;
            let fy = py as f32 + 0.5 - r;
            let dx = if fx < r { r - fx } else if fx > w as f32 - r { fx - (w as f32 - r) } else { 0.0 };
            if dx * dx + fy * fy <= r * r {
                c.put_pixel((x + px) as usize, (y + py) as usize, color);
            }
        }
    }
}

/// A speaker: a box, a cone, and either sound waves for the level or a cross.
fn draw_speaker(c: &Framebuffer, x: i32, cy: i32, color: Color, muted: bool, percent: u32) {
    for py in -4..4 {
        for px in 0..5 {
            c.put_pixel((x + px) as usize, (cy + py) as usize, color);
        }
    }
    for px in 0..8 {
        let half = 4 + px * 5 / 7;
        for py in -half..half {
            c.put_pixel((x + 5 + px) as usize, (cy + py) as usize, color);
        }
    }
    let ox = x + 13;
    if muted {
        for i in 0..9 {
            for t in 0..2 {
                c.put_pixel((ox + 4 + i + t) as usize, (cy - 4 + i) as usize, color);
                c.put_pixel((ox + 4 + i + t) as usize, (cy + 4 - i) as usize, color);
            }
        }
        return;
    }
    let waves = match percent {
        0 => 0,
        1..=33 => 1,
        34..=66 => 2,
        _ => 3,
    };
    for wave in 0..waves {
        let radius = 5.0 + wave as f32 * 5.0;
        for step in 0..64 {
            let angle = -0.9 + 1.8 * step as f32 / 63.0;
            for t in [0.0f32, 0.8] {
                let px = ox as f32 + (radius + t) * angle.cos();
                let py = cy as f32 + (radius + t) * angle.sin();
                c.put_pixel(px as usize, py as usize, color);
            }
        }
    }
}

