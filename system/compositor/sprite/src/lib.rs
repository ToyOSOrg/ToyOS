//! An icon the build drew (`src/icons.rs`), in one colour, and its blit.

pub struct Sprite {
    width: usize,
    height: usize,
    data: Vec<u8>, // RGBA, 4 bytes per pixel
}

impl Sprite {
    /// The icon `/system/share/icons/<stem>.alpha`, in `color`.
    pub fn icon(stem: &str, color: [u8; 3]) -> Self {
        let path = format!("/system/share/icons/{stem}.alpha");
        let file = std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
        let (size, mask) = file.split_at_checked(8).unwrap_or_else(|| panic!("{path} has no header"));
        let width = u32::from_le_bytes(size[..4].try_into().unwrap()) as usize;
        let height = u32::from_le_bytes(size[4..].try_into().unwrap()) as usize;
        assert_eq!(mask.len(), width * height, "{path} is not {width}x{height}");
        let data = mask.iter().flat_map(|&a| [color[0], color[1], color[2], a]).collect();
        Self { width, height, data }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Alpha-blended blit onto a raw pixel buffer.
    /// `pixel_format`: 0 = RGB, 1 = BGR (matches WindowInfo.pixel_format).
    pub fn draw(
        &self,
        dst: *mut u8,
        dst_stride: usize,
        dst_w: usize,
        dst_h: usize,
        pixel_format: u32,
        dx: usize,
        dy: usize,
    ) {
        let bgr = pixel_format != 0;
        for sy in 0..self.height {
            let y = dy + sy;
            if y >= dst_h {
                break;
            }
            for sx in 0..self.width {
                let x = dx + sx;
                if x >= dst_w {
                    break;
                }
                let src_off = (sy * self.width + sx) * 4;
                let alpha = self.data[src_off + 3] as u16;
                if alpha == 0 {
                    continue;
                }
                let dst_off = (y * dst_stride + x) * 4;
                let sr = self.data[src_off] as u16;
                let sg = self.data[src_off + 1] as u16;
                let sb = self.data[src_off + 2] as u16;
                unsafe {
                    let p = dst.add(dst_off);
                    if alpha == 255 {
                        if bgr {
                            *p = sb as u8;
                            *p.add(1) = sg as u8;
                            *p.add(2) = sr as u8;
                        } else {
                            *p = sr as u8;
                            *p.add(1) = sg as u8;
                            *p.add(2) = sb as u8;
                        }
                    } else {
                        let inv = 255 - alpha;
                        let (dr, dg, db) = if bgr {
                            (*p.add(2) as u16, *p.add(1) as u16, *p as u16)
                        } else {
                            (*p as u16, *p.add(1) as u16, *p.add(2) as u16)
                        };
                        let r = ((sr * alpha + dr * inv) / 255) as u8;
                        let g = ((sg * alpha + dg * inv) / 255) as u8;
                        let b = ((sb * alpha + db * inv) / 255) as u8;
                        if bgr {
                            *p = b;
                            *p.add(1) = g;
                            *p.add(2) = r;
                        } else {
                            *p = r;
                            *p.add(1) = g;
                            *p.add(2) = b;
                        }
                    }
                }
            }
        }
    }
}
