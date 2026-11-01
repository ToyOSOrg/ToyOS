use std::fs;
use std::path::PathBuf;

use toyos_icon::{Icon, Mask};
use window::{Color, Framebuffer, Scale, Window};

const BG: Color = Color { r: 0x1e, g: 0x1e, b: 0x2e };
const TEXT_COLOR: Color = Color { r: 0xe0, g: 0xe0, b: 0xe8 };
const DIM_TEXT: Color = Color { r: 0x70, g: 0x70, b: 0x80 };
const SELECTED_BG: Color = Color { r: 0x35, g: 0x35, b: 0x55 };
const PATH_BG: Color = Color { r: 0x15, g: 0x15, b: 0x22 };
const PATH_COLOR: Color = Color { r: 0x80, g: 0x80, b: 0x90 };
const SCROLLBAR_TRACK: Color = Color { r: 0x25, g: 0x25, b: 0x35 };
const SCROLLBAR_THUMB: Color = Color { r: 0x55, g: 0x55, b: 0x65 };

const FOLDER_COLOR: [u8; 3] = [0xf0, 0xc8, 0x50];
const FILE_COLOR: [u8; 3] = [0xd0, 0xd0, 0xd8];

/// The desktop's monospace font and the icons, which the browser draws at its
/// window's scale.
const FONT: &str = "/system/share/fonts/JetBrainsMono-Regular.ttf";
const ICONS_DIR: &str = "/system/share/icons";

/// Every distance the browser lays out, in its window's pixels: each a
/// logical one at the window's scale.
struct Metrics {
    item_width: usize,
    item_height: usize,
    padding: usize,
    path_bar_height: usize,
    status_bar_height: usize,
    scrollbar_width: usize,
    thumb_min: usize,
    /// From a bar's edge to its text, and from an icon to its name.
    inset: usize,
    gap: usize,
    icon: usize,
}

impl Metrics {
    fn at(scale: Scale) -> Self {
        let px = |l: u32| scale.px_u32(l) as usize;
        Self {
            item_width: px(80),
            item_height: px(64),
            padding: px(8),
            path_bar_height: px(24),
            status_bar_height: px(20),
            scrollbar_width: px(6),
            thumb_min: px(20),
            inset: px(8),
            gap: px(2),
            icon: px(32),
        }
    }
}

/// A surface an icon's mask is blended onto.
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

/// What the browser draws with, as the image ships it.
struct Shapes {
    font: Vec<u8>,
    folder: Icon,
    file: Icon,
}

impl Shapes {
    fn read() -> Self {
        let icon = |stem: &str| {
            let path = format!("{ICONS_DIR}/{stem}.svg");
            Icon::parse(&fs::read_to_string(&path).unwrap_or_else(|e| panic!("files: {path}: {e}")))
        };
        Self {
            font: fs::read(FONT).unwrap_or_else(|e| panic!("files: {FONT}: {e}")),
            folder: icon("folder-bold"),
            file: icon("file-bold"),
        }
    }
}

/// The font, the icons and the metrics at one scale.
struct Drawn {
    scale: Scale,
    m: Metrics,
    font: font::Font,
    folder: Mask,
    file: Mask,
}

impl Drawn {
    fn at(shapes: &Shapes, scale: Scale) -> Self {
        let m = Metrics::at(scale);
        let (w, h) = scale.cell();
        let font = font::Font::from_ttf(&shapes.font, w, h).unwrap_or_else(|e| panic!("files: {FONT}: {e}"));
        Self { scale, folder: shapes.folder.mask(m.icon), file: shapes.file.mask(m.icon), font, m }
    }
}

struct Entry {
    name: String,
    is_dir: bool,
    size: u64,
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.0}K", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}M", bytes as f64 / (1024.0 * 1024.0))
    }
}

struct FileBrowser {
    window: Window,
    fb: Framebuffer<'static>,
    shapes: Shapes,
    drawn: Drawn,
    entries: Vec<Entry>,
    selected: Option<usize>,
    current_dir: PathBuf,
    scroll_y: usize,
}

impl FileBrowser {
    fn new() -> Self {
        let window = Window::create_with_title(window::Size::Any, "Files").unwrap_or_else(|e| {
            eprintln!("files: {e}");
            std::process::exit(1);
        });
        let fb = window.framebuffer();
        let shapes = Shapes::read();
        let drawn = Drawn::at(&shapes, window.scale());

        let current_dir = std::env::home_dir().expect("files: the supervisor starts every program with HOME");

        let mut browser = Self {
            window,
            fb,
            shapes,
            drawn,
            entries: Vec::new(),
            selected: None,
            current_dir,
            scroll_y: 0,
        };
        browser.load_directory();
        browser.redraw();
        browser.window.present();
        browser
    }

    fn load_directory(&mut self) {
        self.entries.clear();
        self.selected = None;
        self.scroll_y = 0;

        if self.current_dir.as_os_str() != "/" {
            self.entries.push(Entry {
                name: "..".to_string(),
                is_dir: true,
                size: 0,
            });
        }

        if let Ok(read_dir) = fs::read_dir(&self.current_dir) {
            let mut items: Vec<Entry> = read_dir
                .filter_map(|e| e.ok())
                .map(|e| {
                    let is_dir = e.file_type().map_or(false, |ft| ft.is_dir());
                    let size = if is_dir { 0 } else { e.metadata().map_or(0, |m| m.len()) };
                    Entry {
                        name: e.file_name().to_string_lossy().into_owned(),
                        is_dir,
                        size,
                    }
                })
                .collect();
            items.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
            self.entries.append(&mut items);
        }
    }

    fn cols(&self) -> usize {
        let (w, m) = (self.fb.width(), &self.drawn.m);
        if w > m.padding { (w - m.padding) / m.item_width } else { 1 }
    }

    fn content_height(&self) -> usize {
        let cols = self.cols();
        let rows = self.entries.len().div_ceil(cols);
        rows * self.drawn.m.item_height
    }

    fn visible_height(&self) -> usize {
        let m = &self.drawn.m;
        self.fb.height().saturating_sub(m.path_bar_height + m.padding + m.status_bar_height)
    }

    fn max_scroll(&self) -> usize {
        self.content_height().saturating_sub(self.visible_height())
    }

    fn redraw(&self) {
        let w = self.fb.width();
        let h = self.fb.height();

        self.fb.fill_rect(0, 0, w, h, BG);

        let (m, font) = (&self.drawn.m, &self.drawn.font);
        self.fb.fill_rect(0, 0, w, m.path_bar_height, PATH_BG);
        let path_str = self.current_dir.display().to_string();
        let max_chars = w.saturating_sub(2 * m.inset) / font.width();
        let display_path: String = path_str.chars().take(max_chars).collect();
        let text_y = m.path_bar_height.saturating_sub(font.height()) / 2;
        font.draw_string(&self.fb, m.inset, text_y, &display_path, PATH_COLOR, PATH_BG);

        let cols = self.cols();
        let content_top = m.path_bar_height + m.padding;
        let content_bottom = h.saturating_sub(m.status_bar_height);

        for (i, entry) in self.entries.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let x = m.padding + col * m.item_width;
            let y = content_top + row * m.item_height - self.scroll_y;

            if y + m.item_height <= content_top || y >= content_bottom {
                continue;
            }

            if self.selected == Some(i) {
                let clip_y = y.max(content_top);
                let clip_h = (y + m.item_height).min(content_bottom) - clip_y;
                self.fb.fill_rect(x, clip_y, m.item_width, clip_h, SELECTED_BG);
            }

            let (icon, color) =
                if entry.is_dir { (&self.drawn.folder, FOLDER_COLOR) } else { (&self.drawn.file, FILE_COLOR) };
            if y >= content_top && y + icon.edge() <= content_bottom {
                let icon_x = x + (m.item_width - icon.edge()) / 2;
                icon.draw(&Canvas(&self.fb), icon_x as i64, y as i64, color);
            }

            let text_bg = if self.selected == Some(i) { SELECTED_BG } else { BG };

            let max_chars = m.item_width / font.width();
            let name: String = entry.name.chars().take(max_chars).collect();
            let text_x = x + (m.item_width.saturating_sub(name.len() * font.width())) / 2;
            let text_y = y + icon.edge() + m.gap;
            if text_y >= content_top && text_y + font.height() <= content_bottom {
                font.draw_string(&self.fb, text_x, text_y, &name, TEXT_COLOR, text_bg);
            }

            // File size (for files only)
            if !entry.is_dir && entry.name != ".." {
                let size_str = format_size(entry.size);
                let size_x = x + (m.item_width.saturating_sub(size_str.len() * font.width())) / 2;
                let size_y = text_y + font.height();
                if size_y >= content_top && size_y + font.height() <= content_bottom {
                    font.draw_string(&self.fb, size_x, size_y, &size_str, DIM_TEXT, text_bg);
                }
            }
        }

        let status_y = h - m.status_bar_height;
        self.fb.fill_rect(0, status_y, w, m.status_bar_height, PATH_BG);
        let file_count = self.entries.iter().filter(|e| e.name != "..").count();
        let status = format!("{} items", file_count);
        let text_y = status_y + m.status_bar_height.saturating_sub(font.height()) / 2;
        font.draw_string(&self.fb, m.inset, text_y, &status, PATH_COLOR, PATH_BG);

        if self.max_scroll() > 0 {
            let visible = self.visible_height();
            let total = self.content_height();
            let track_x = w - m.scrollbar_width;
            let track_top = content_top;
            let track_height = content_bottom - content_top;
            self.fb.fill_rect(track_x, track_top, m.scrollbar_width, track_height, SCROLLBAR_TRACK);

            let thumb_height = (track_height * visible / total).max(m.thumb_min);
            let track_range = track_height.saturating_sub(thumb_height);
            let thumb_top = if self.max_scroll() > 0 {
                track_top + self.scroll_y * track_range / self.max_scroll()
            } else {
                track_top
            };
            self.fb.fill_rect(track_x, thumb_top, m.scrollbar_width, thumb_height, SCROLLBAR_THUMB);
        }
    }

    fn item_at(&self, mx: usize, my: usize) -> Option<usize> {
        let m = &self.drawn.m;
        let content_y = m.path_bar_height + m.padding;
        if my < content_y || my >= self.fb.height().saturating_sub(m.status_bar_height) {
            return None;
        }
        let cols = self.cols();
        let col = mx.checked_sub(m.padding)? / m.item_width;
        let row = (my - content_y + self.scroll_y) / m.item_height;
        if col >= cols {
            return None;
        }
        let idx = row * cols + col;
        if idx < self.entries.len() {
            Some(idx)
        } else {
            None
        }
    }

    fn open(&mut self, idx: usize) {
        let entry = &self.entries[idx];
        if entry.is_dir {
            if entry.name == ".." {
                if let Some(parent) = self.current_dir.parent() {
                    self.current_dir = parent.to_path_buf();
                }
            } else {
                self.current_dir = self.current_dir.join(&entry.name);
            }
            self.load_directory();
            self.redraw();
            self.window.present();
        }
    }

    fn ensure_visible(&mut self, idx: usize) {
        let cols = self.cols();
        let row = idx / cols;
        let item_top = row * self.drawn.m.item_height;
        let item_bottom = item_top + self.drawn.m.item_height;
        let visible = self.visible_height();

        if item_top < self.scroll_y {
            self.scroll_y = item_top;
        } else if item_bottom > self.scroll_y + visible {
            self.scroll_y = item_bottom.saturating_sub(visible);
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.entries.len();
        if len == 0 {
            return;
        }
        let new = match self.selected {
            Some(cur) => (cur as isize + delta).clamp(0, len as isize - 1) as usize,
            None => 0,
        };
        self.selected = Some(new);
        self.ensure_visible(new);
        self.redraw();
        self.window.present();
    }

    fn run(&mut self) {
        loop {
            match self.window.recv_event() {
                window::Event::MouseInput(ev) => {
                    if ev.event_type == window::MOUSE_PRESS && ev.changed == 1 {
                        if let Some(idx) = self.item_at(ev.x as usize, ev.y as usize) {
                            if self.selected == Some(idx) {
                                self.open(idx);
                            } else {
                                self.selected = Some(idx);
                                self.redraw();
                                self.window.present();
                            }
                        }
                    } else if ev.event_type == window::MOUSE_SCROLL {
                        let step = self.drawn.m.item_height;
                        if ev.scroll < 0 {
                            self.scroll_y = (self.scroll_y + step).min(self.max_scroll());
                        } else if ev.scroll > 0 {
                            self.scroll_y = self.scroll_y.saturating_sub(step);
                        }
                        self.redraw();
                        self.window.present();
                    }
                }
                window::Event::KeyInput(ev) if ev.pressed() => {
                    let cols = self.cols() as isize;
                    match ev.keycode {
                        0x51 => self.move_selection(cols),         // Down arrow
                        0x52 => self.move_selection(-cols),        // Up arrow
                        0x4F => self.move_selection(1),            // Right arrow
                        0x50 => self.move_selection(-1),           // Left arrow
                        0x2A => {
                            // Backspace: go up
                            if self.current_dir.as_os_str() != "/" {
                                if let Some(parent) = self.current_dir.parent() {
                                    self.current_dir = parent.to_path_buf();
                                }
                                self.load_directory();
                                self.redraw();
                                self.window.present();
                            }
                        }
                        0x28 => {
                            // Enter: open selected
                            if let Some(idx) = self.selected {
                                self.open(idx);
                            }
                        }
                        _ => {}
                    }
                }
                window::Event::KeyInput(_) => {}
                window::Event::Resized => {
                    self.fb = self.window.framebuffer();
                    if self.window.scale() != self.drawn.scale {
                        self.drawn = Drawn::at(&self.shapes, self.window.scale());
                    }
                    self.scroll_y = self.scroll_y.min(self.max_scroll());
                    self.redraw();
                    self.window.present();
                }
                window::Event::ClipboardPaste(_) => {}
                // Every key this browser reads is a HID usage — arrows, Enter,
                // Backspace — so no layout moves any of them.
                window::Event::LayoutChanged => {}
                window::Event::Frame => {}
                window::Event::Close => break,
            }
        }
    }
}

fn main() {
    let mut browser = FileBrowser::new();
    browser.run();
}
