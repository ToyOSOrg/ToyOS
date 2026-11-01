//! Turning a compose plan into pixels.
//!
//! **Nothing here touches the scanout.** Everything composes into the back
//! buffer, a [`Framebuffer`] of one screen of system RAM, and the finished rectangles are
//! handed to the panel whole — which is why nothing on screen is ever
//! half-composed, and why `fill_rect` no longer reads the row it is
//! replicating back out of a mapping firmware made uncached.
//!
//! Every decision this module acts on — what is visible, in what order, clipped
//! to what, which pixels of a client's buffer reach the screen — is
//! `toyos_desktop`'s and host-tested there. What is left here is the drawing.

use toyos_desktop::{content_blit, plan, Desk, Layer, Level, Rect, Stack, PROMPT_FRAME};
use toyos_icon::Mask;
use window::{Color, Framebuffer};

use crate::client::Client;

pub const FOCUSED_TITLE_COLOR: Color = Color { r: 0x3a, g: 0x3a, b: 0x4e };
pub const UNFOCUSED_TITLE_COLOR: Color = Color { r: 0x28, g: 0x28, b: 0x32 };
pub const FOCUSED_BORDER_COLOR: Color = Color { r: 0x58, g: 0x58, b: 0x6e };
pub const UNFOCUSED_BORDER_COLOR: Color = Color { r: 0x38, g: 0x38, b: 0x42 };
pub const FOCUSED_TITLE_TEXT: Color = Color { r: 0xe0, g: 0xe0, b: 0xe8 };
pub const UNFOCUSED_TITLE_TEXT: Color = Color { r: 0x60, g: 0x60, b: 0x70 };
pub const CLOSE_BUTTON_BG: Color = Color { r: 0x50, g: 0x28, b: 0x28 };
pub const TASKBAR_COLOR: Color = Color { r: 0x18, g: 0x18, b: 0x25 };
pub const TASKBAR_ACTIVE_COLOR: Color = Color { r: 0x30, g: 0x30, b: 0x45 };
pub const TASKBAR_TEXT_COLOR: Color = Color { r: 0x80, g: 0x80, b: 0x90 };
pub const TASKBAR_ACTIVE_TEXT: Color = Color { r: 0xe0, g: 0xe0, b: 0xe8 };
pub const TASKBAR_NEW_COLOR: Color = Color { r: 0x40, g: 0x60, b: 0x40 };
pub const TASKBAR_NEW_TEXT: Color = Color { r: 0x80, g: 0xc0, b: 0x80 };
pub const TASKBAR_MINIMIZED_COLOR: Color = Color { r: 0x20, g: 0x20, b: 0x30 };
pub const TASKBAR_MINIMIZED_TEXT: Color = Color { r: 0x50, g: 0x50, b: 0x60 };
pub const LAUNCHER_BG: Color = Color { r: 0x20, g: 0x20, b: 0x30 };
pub const LAUNCHER_TEXT: Color = Color { r: 0xe0, g: 0xe0, b: 0xe8 };
pub const PROMPT_TITLE_TEXT: Color = Color { r: 0x1e, g: 0x1e, b: 0x2e };

/// What a prompt's title bar says, whatever its client asked for.
const PROMPT_TITLE: &str = "ToyOS is asking";

/// The title-bar buttons' icons at the desk's scale, drawn white.
pub struct TitleBarIcons {
    pub minimize: Mask,
    pub maximize: Mask,
    pub close: Mask,
}

const ICON_COLOR: [u8; 3] = [255, 255, 255];

/// A surface a [`Mask`] is blended onto.
struct Canvas<'a>(&'a Framebuffer<'a>);

impl toyos_icon::Canvas for Canvas<'_> {
    fn size(&self) -> (usize, usize) {
        (self.0.width(), self.0.height())
    }

    fn get(&self, x: usize, y: usize) -> [u8; 3] {
        let c = self.0.get_pixel(x, y);
        [c.r, c.g, c.b]
    }

    fn put(&self, x: usize, y: usize, [r, g, b]: [u8; 3]) {
        self.0.put_pixel(x, y, Color { r, g, b });
    }
}

pub struct SystemStats {
    pub used_mb: u64,
    pub total_mb: u64,
    pub cpu_pct: u64,
}

/// Everything the renderer needs that is not state.
pub struct Assets<'a> {
    pub font: &'a font::Font,
    pub icons: &'a TitleBarIcons,
    pub wallpaper: &'a [u8],
    pub apps: &'a [(String, String)],
}

fn fill(surface: &Framebuffer, r: Rect, color: Color) {
    if r.is_empty() {
        return;
    }
    surface.fill_rect(r.x0 as usize, r.y0 as usize, r.w() as usize, r.h() as usize, color);
}

/// Compose `region` of the desktop into `back`.
pub fn paint(
    back: &Framebuffer,
    desk: &Desk,
    stack: &Stack<Client>,
    assets: &Assets,
    launcher_open: bool,
    stats: &SystemStats,
    region: Rect,
) {
    let focused = stack.focused();
    for layer in plan::compose(desk, stack, region, launcher_open) {
        match layer {
            Layer::Wallpaper(r) => {
                let offset = (r.y0 as usize * back.width() + r.x0 as usize) * 4;
                back.blit(
                    r.x0 as usize,
                    r.y0 as usize,
                    r.w() as usize,
                    r.h() as usize,
                    back.width(),
                    &assets.wallpaper[offset..],
                );
            }
            Layer::Window { index, clip } => {
                draw_window(back, desk, &stack[index], Some(index) == focused, assets, clip)
            }
            Layer::Taskbar { clip } => draw_taskbar(back, desk, stack, focused, assets, stats, clip),
            Layer::Launcher(r) => draw_launcher(back, desk, stack.len(), assets, r),
        }
    }
}

fn draw_window(
    surface: &Framebuffer,
    desk: &Desk,
    win: &crate::client::Win,
    focused: bool,
    assets: &Assets,
    clip: Rect,
) {
    let chrome = &desk.chrome;
    // A prompt's frame is the compositor's own: its colour, its title and no
    // buttons, since nothing but its answer takes it down.
    let prompt = win.level == Level::Prompt;
    let [r, g, b] = PROMPT_FRAME;
    let (border_color, title_color, text_color) = match (prompt, focused) {
        (true, _) => (Color { r, g, b }, Color { r, g, b }, PROMPT_TITLE_TEXT),
        (false, true) => (FOCUSED_BORDER_COLOR, FOCUSED_TITLE_COLOR, FOCUSED_TITLE_TEXT),
        (false, false) => (UNFOCUSED_BORDER_COLOR, UNFOCUSED_TITLE_COLOR, UNFOCUSED_TITLE_TEXT),
    };

    let frame = win.frame(chrome);
    let strip = chrome.title_strip(frame);
    if clip.overlaps(strip) {
        fill(surface, strip, border_color);
        // The title bar's own colour, inset by the border on three sides. Its
        // bottom edge is the content's top edge: a window's chrome has no rule
        // between the title and what the client draws.
        let bar = Rect::corners(
            strip.x0 + chrome.border,
            strip.y0 + chrome.border,
            strip.x1 - chrome.border,
            strip.y1,
        );
        fill(surface, bar, title_color);

        let title = match (prompt, win.title.is_empty()) {
            (true, _) => PROMPT_TITLE,
            (false, true) => "Window",
            (false, false) => &win.title,
        };
        assets.font.draw_string(
            surface,
            (bar.x0 + chrome.text_inset) as usize,
            (bar.y0 + (chrome.title_bar - assets.font.height() as i32) / 2) as usize,
            title,
            text_color,
            title_color,
        );

        let [close, maximize, minimize] = chrome.buttons(frame);
        let close_bg = if focused { CLOSE_BUTTON_BG } else { title_color };
        let buttons = [
            (close, close_bg, &assets.icons.close),
            (maximize, title_color, &assets.icons.maximize),
            (minimize, title_color, &assets.icons.minimize),
        ];
        for (rect, bg, icon) in buttons.into_iter().filter(|_| !prompt) {
            fill(surface, rect, bg);
            draw_icon_centered(surface, icon, rect);
        }
    }

    // Side and bottom borders, each clipped: the colour does not depend on the
    // region, so drawing them outside it would be correct and wasted.
    for edge in [
        Rect::corners(frame.x0, win.content.y0, win.content.x0, win.content.y1),
        Rect::corners(win.content.x1, win.content.y0, frame.x1, win.content.y1),
        Rect::corners(frame.x0, win.content.y1, frame.x1, frame.y1),
    ] {
        fill(surface, edge.intersect(clip), border_color);
    }

    if let Some(b) = content_blit(win, clip) {
        let buffer = win.client.buffers.shown().shm.bytes();
        let offset = (b.src_y as usize * win.buf_w as usize + b.src_x as usize) * 4;
        surface.blit_shared(
            b.dst.x0 as usize,
            b.dst.y0 as usize,
            b.dst.w() as usize,
            b.dst.h() as usize,
            win.buf_w as usize,
            buffer.sub(offset, buffer.len().saturating_sub(offset)),
        );
    }
}

fn draw_icon_centered(surface: &Framebuffer, icon: &Mask, area: Rect) {
    let ix = area.x0 as usize + (area.w() as usize).saturating_sub(icon.edge()) / 2;
    let iy = area.y0 as usize + (area.h() as usize).saturating_sub(icon.edge()) / 2;
    icon.draw(&Canvas(surface), ix as i64, iy as i64, ICON_COLOR);
}

/// Paint the parts of the taskbar `clip` reaches.
///
/// Every element tests `clip` for itself, so the clock's second costs the
/// status box and not a bar's worth of tabs and titles.
fn draw_taskbar(
    back: &Framebuffer,
    desk: &Desk,
    stack: &Stack<Client>,
    focused: Option<usize>,
    assets: &Assets,
    stats: &SystemStats,
    clip: Rect,
) {
    let bar = desk.taskbar(stack.len());
    let glyph_h = assets.font.height() as i32;
    let text_y = (bar.strip().y0 + (desk.chrome.taskbar - glyph_h) / 2) as usize;
    let inset = desk.chrome.text_inset;

    let gap = bar.gap();
    if clip.overlaps(gap) {
        fill(back, gap, TASKBAR_COLOR);
    }

    for (i, win) in stack.iter().enumerate() {
        let tab = bar.tab(i);
        if !clip.overlaps(tab) {
            continue;
        }
        let (bg, fg) = if win.minimized {
            (TASKBAR_MINIMIZED_COLOR, TASKBAR_MINIMIZED_TEXT)
        } else if Some(i) == focused {
            (TASKBAR_ACTIVE_COLOR, TASKBAR_ACTIVE_TEXT)
        } else {
            (TASKBAR_COLOR, TASKBAR_TEXT_COLOR)
        };
        fill(back, tab, TASKBAR_COLOR);
        fill(back, bar.tab_face(i), bg);
        let max_chars = (desk.chrome.taskbar_item - 2 * inset).max(0) as usize / assets.font.width();
        let title = if win.title.is_empty() { "Window" } else { &win.title };
        let display: String = title.chars().take(max_chars).collect();
        assets.font.draw_string(back, (tab.x0 + inset) as usize, text_y, &display, fg, bg);
    }

    let plus = bar.new_button();
    if clip.overlaps(plus) {
        fill(back, plus, TASKBAR_COLOR);
        fill(back, bar.new_button_face(), TASKBAR_NEW_COLOR);
        let plus_x = (plus.x0 + (desk.chrome.taskbar - desk.font_w) / 2) as usize;
        assets.font.draw_char(back, plus_x, text_y, '+', TASKBAR_NEW_TEXT, TASKBAR_NEW_COLOR);
    }

    let status = bar.status();
    if clip.overlaps(status) {
        // Dashes and not 00:00 on a machine whose clock never answered: a
        // reader cannot tell a plausible midnight from a missing clock.
        let clock = match toyos::system::clock_epoch() {
            Some(secs) => {
                let now = toyos_wallclock::Civil::from_unix_secs(secs);
                format!("{:02}:{:02}", now.hour, now.min)
            }
            None => String::from("--:--"),
        };
        let text: String =
            format!("{}M/{}M  CPU {}%  {clock}", stats.used_mb, stats.total_mb, stats.cpu_pct)
                .chars()
                .take(toyos_desktop::MAX_STATUS_CHARS)
                .collect();
        fill(back, status, TASKBAR_COLOR);
        let text_w = text.chars().count() * assets.font.width();
        let x = (status.x1 as usize).saturating_sub(desk.chrome.status_margin as usize + text_w);
        assets.font.draw_string(back, x, text_y, &text, TASKBAR_ACTIVE_TEXT, TASKBAR_COLOR);
    }
}

fn draw_launcher(
    surface: &Framebuffer,
    desk: &Desk,
    windows: usize,
    assets: &Assets,
    area: Rect,
) {
    fill(surface, area, LAUNCHER_BG);
    let bar = desk.taskbar(windows);
    for (i, (name, _)) in assets.apps.iter().enumerate() {
        let row = bar.launcher_item(i);
        assets.font.draw_string(
            surface,
            (row.x0 + desk.chrome.launcher_inset) as usize,
            (row.y0 + (row.h() - assets.font.height() as i32) / 2) as usize,
            name,
            LAUNCHER_TEXT,
            LAUNCHER_BG,
        );
    }
}

/// The volume overlay: `text` centred in `rect`. Drawn whole whenever a
/// region touches it, so a region that cuts it is repainted whole.
pub fn draw_overlay(surface: &Framebuffer, font: &font::Font, rect: Rect, text: &str) {
    fill(surface, rect, LAUNCHER_BG);
    let text_w = (text.chars().count() * font.width()) as i32;
    let x = rect.x0 + (rect.w() - text_w).max(0) / 2;
    let y = rect.y0 + (rect.h() - font.height() as i32) / 2;
    font.draw_string(surface, x as usize, y as usize, text, LAUNCHER_TEXT, LAUNCHER_BG);
}

/// Scale an RGB image to the screen and convert to its pixel format.
pub fn scale_wallpaper(
    src: &[u8],
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
    bgr: bool,
) -> Vec<u8> {
    let mut dst = vec![0u8; dst_w * dst_h * 4];
    for y in 0..dst_h {
        let sy = y * src_h / dst_h;
        for x in 0..dst_w {
            let sx = x * src_w / dst_w;
            let si = (sy * src_w + sx) * 3;
            let di = (y * dst_w + x) * 4;
            if bgr {
                dst[di] = src[si + 2];
                dst[di + 1] = src[si + 1];
                dst[di + 2] = src[si];
            } else {
                dst[di] = src[si];
                dst[di + 1] = src[si + 1];
                dst[di + 2] = src[si + 2];
            }
        }
    }
    dst
}

/// The pixels of a hardware cursor's image, 64 by 64.
pub const CURSOR_PIXELS: usize = (toyos_desktop::cursor::HARDWARE_EDGE * toyos_desktop::cursor::HARDWARE_EDGE) as usize;

/// A cursor's mask in `color` as a hardware cursor's image, whose pixels'
/// bytes are blue, green, red and alpha.
pub fn cursor_image(mask: &Mask, color: [u8; 3]) -> [u32; CURSOR_PIXELS] {
    const EDGE: usize = toyos_desktop::cursor::HARDWARE_EDGE as usize;
    let edge = mask.edge();
    assert!(edge <= EDGE, "a {edge}-pixel cursor is drawn in software");
    let mut image = [0u32; CURSOR_PIXELS];
    for (i, &alpha) in mask.alpha().iter().enumerate() {
        image[(i / edge) * EDGE + i % edge] = u32::from_le_bytes([color[2], color[1], color[0], alpha]);
    }
    image
}

/// Draw a cursor's mask into the composed frame, its top-left corner at
/// `at`, when the display's cursor plane does not carry it.
///
/// It blends, so it reads the pixel under every partly covered one — which
/// is why it draws into the back buffer and not the panel.
pub fn draw_software_cursor(surface: &Framebuffer, mask: &Mask, color: [u8; 3], at: toyos_desktop::Point) {
    mask.draw(&Canvas(surface), at.x.into(), at.y.into(), color);
}
