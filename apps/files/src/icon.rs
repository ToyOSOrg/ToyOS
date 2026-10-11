//! An icon the build drew (`build/icons.rs`), in one colour, and its blit.

use window::{Color, Framebuffer};

pub struct Icon {
    width: usize,
    height: usize,
    color: Color,
    alpha: Vec<u8>,
}

impl Icon {
    /// The icon `/system/share/icons/<stem>.alpha`, in `color`.
    pub fn load(stem: &str, color: Color) -> Self {
        let path = format!("/system/share/icons/{stem}.alpha");
        let file = std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
        let (size, alpha) = file.split_at_checked(8).unwrap_or_else(|| panic!("{path} has no header"));
        let width = u32::from_le_bytes(size[..4].try_into().unwrap()) as usize;
        let height = u32::from_le_bytes(size[4..].try_into().unwrap()) as usize;
        assert_eq!(alpha.len(), width * height, "{path} is not {width}x{height}");
        Self { width, height, color, alpha: alpha.to_vec() }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// Alpha-blended onto `fb`, its top-left corner at (`dx`, `dy`).
    pub fn draw(&self, fb: &Framebuffer, dx: usize, dy: usize) {
        for (i, &alpha) in self.alpha.iter().enumerate() {
            if alpha == 0 {
                continue;
            }
            let (x, y) = (dx + i % self.width, dy + i / self.width);
            let under = fb.get_pixel(x, y);
            let (a, inv) = (u16::from(alpha), u16::from(255 - alpha));
            let blend = |src: u8, dst: u8| ((u16::from(src) * a + u16::from(dst) * inv) / 255) as u8;
            let c = self.color;
            fb.put_pixel(x, y, Color { r: blend(c.r, under.r), g: blend(c.g, under.g), b: blend(c.b, under.b) });
        }
    }
}
