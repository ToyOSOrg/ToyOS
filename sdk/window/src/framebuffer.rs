use core::cell::Cell;
use core::marker::PhantomData;
use core::ptr;
use std::rc::Rc;

use toyos::shm::SharedMemory;

use crate::Rect;

pub use font::Color;

#[derive(Clone, Copy, PartialEq)]
enum PixelFormat {
    Rgb,
    Bgr,
}

impl PixelFormat {
    fn from_raw(raw: u32) -> Self {
        if raw == 0 { Self::Rgb } else { Self::Bgr }
    }

    #[inline]
    fn encode(self, color: Color) -> [u8; 4] {
        match self {
            Self::Rgb => [color.r, color.g, color.b, 0],
            Self::Bgr => [color.b, color.g, color.r, 0],
        }
    }
}

/// The scanout, which may only be written to.
///
/// [`Framebuffer`] reads as freely as it writes, which is what a pixel surface
/// in system RAM should be. A GOP scanout is not one: the kernel maps it
/// write-combining, so a write is a posted store that merges with its
/// neighbours on the way out, while a *read* misses every cache and costs a
/// bus transaction. Composing against the panel therefore pays that per byte
/// read back. QEMU's framebuffer is host RAM, where none of it exists, so no
/// test can show it.
///
/// The property is that nothing reads the panel, and this type is how it is
/// held rather than promised: `Screen` hands out no pointer and returns no
/// pixel, so there is nothing to read through. Its one drawing operation takes
/// pixels that were composed somewhere else, each row one copy into the
/// mapping it owns.
pub struct Screen {
    memory: Box<dyn AsRef<SharedMemory>>,
    width: usize,
    height: usize,
    stride: usize,
    pixel_format: PixelFormat,
    /// What this mapping has cost, for a machine whose only instrument is the
    /// panel it is being asked about.
    written: Cell<u64>,
    blits: Cell<u64>,
    /// Where the blits since the last [`Screen::take_damage`] landed.
    ///
    /// The surface is the one thing that sees every blit, so it is the one
    /// thing that can answer "what changed" without its owner keeping a second
    /// account that can disagree with the pixels.
    damage: Cell<Option<Rect>>,
}

impl Screen {
    /// Over the first `stride * height` pixels of what `memory` maps: a
    /// scanout, or a window's buffer.
    pub fn new(memory: impl AsRef<SharedMemory> + 'static, width: usize, height: usize, stride: usize, pixel_format: u32) -> Self {
        let len = memory.as_ref().len();
        assert!(
            stride >= width && pixels(stride, height).is_some_and(|bytes| bytes <= len),
            "Screen: {width}x{height} at stride {stride} over {len} bytes"
        );
        Self {
            memory: Box::new(memory),
            width,
            height,
            stride,
            pixel_format: PixelFormat::from_raw(pixel_format),
            written: Cell::new(0),
            blits: Cell::new(0),
            damage: Cell::new(None),
        }
    }

    /// The area every blit since the last call landed in, and `None` when
    /// there has been none.
    ///
    /// One rectangle rather than a list: what asks is a client about to name
    /// its damage to the compositor, and one message describing one box is
    /// cheaper than the pixels a second box would save at the sizes a client
    /// dirties between two frames.
    pub fn take_damage(&self) -> Option<Rect> {
        self.damage.take()
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn pixel_format_raw(&self) -> u32 {
        if self.pixel_format == PixelFormat::Rgb { 0 } else { 1 }
    }

    /// Bytes handed to the panel, and the number of blits that carried them.
    pub fn traffic(&self) -> (u64, u64) {
        (self.written.get(), self.blits.get())
    }

    /// Copy `h` rows of `w` pixels from `(src_x, src_y)` of `src` to
    /// `(x, y)`, one copy per scanline.
    ///
    /// Rows and columns clipped away by the screen are not copied, and a
    /// `src` too small for the rectangle that is left is a caller bug rather
    /// than a partial paint.
    #[allow(clippy::too_many_arguments, reason = "two rectangles' corners and a size")]
    pub fn blit(&self, x: usize, y: usize, w: usize, h: usize, src: &Framebuffer, src_x: usize, src_y: usize) {
        let w = w.min(self.width.saturating_sub(x));
        let h = h.min(self.height.saturating_sub(y));
        if w == 0 || h == 0 {
            return;
        }
        let inside = |at: usize, len: usize, of: usize| at.checked_add(len).is_some_and(|end| end <= of);
        assert!(
            inside(src_x, w, src.width) && inside(src_y, h, src.height),
            "Screen::blit: {w}x{h} at ({src_x}, {src_y}) of a {}x{} source",
            src.width,
            src.height
        );
        let row_bytes = w * 4;
        let mapping = (*self.memory).as_ref();
        let (panel, source) = (mapping.as_ptr() as usize, src.buf as usize);
        assert!(
            source + src.len <= panel || panel + mapping.len() <= source,
            "Screen::blit: the source is in the screen's own mapping"
        );
        let panel = mapping.bytes();
        for dy in 0..h {
            let row = panel.sub(((y + dy) * self.stride + x) * 4, row_bytes);
            // SAFETY: the first assertion kept the row inside `src`, the
            // second kept `src` outside the panel's mapping, and no reference
            // reaches a framebuffer's memory outside its own calls.
            unsafe { row.write_from(src.at(src_x, src_y + dy)) }
        }
        // A scanout is write-combining, so the tail of the last row can sit in
        // a store buffer until something unrelated evicts it — a sliver of the
        // previous frame left on the panel for as long as its owner has nothing
        // else to draw. This is the call that says the pixels are the
        // surface's now.
        crate::arch::drain_stores();
        self.written.set(self.written.get() + (row_bytes * h) as u64);
        self.blits.set(self.blits.get() + 1);
        let painted = Rect { x: x as u32, y: y as u32, w: w as u32, h: h as u32 };
        self.damage.set(Some(match self.damage.get() {
            Some(prev) => prev.union(painted),
            None => painted,
        }));
    }
}

/// `stride * height` pixels' bytes, unless that overflows.
fn pixels(stride: usize, height: usize) -> Option<usize> {
    stride.checked_mul(height)?.checked_mul(4)
}

/// A pixel surface drawn with plain loads and stores, through `&self`: no
/// reference reaches its memory outside its own calls, and it is neither
/// `Send` nor `Sync`, so every surface over the same memory is on one thread
/// and no two calls on them run at once.
pub struct Framebuffer<'a> {
    buf: *mut u8,
    len: usize,
    width: usize,
    height: usize,
    stride: usize,
    pixel_format: PixelFormat,
    _memory: Memory<'a>,
}

/// What keeps a [`Framebuffer`]'s pixels alive.
enum Memory<'a> {
    Own { _pixels: Vec<u8> },
    Borrowed { _pixels: PhantomData<&'a mut [u8]> },
    Mapped { _region: Rc<SharedMemory> },
}

impl Framebuffer<'static> {
    /// `width` by `height` black pixels of its own.
    pub fn new(width: usize, height: usize, pixel_format: u32) -> Self {
        let mut own = vec![0; pixels(width, height).expect("Framebuffer: a size no allocation holds")];
        let buf = own.as_mut_ptr();
        Self::over_raw(buf, own.len(), width, height, width, pixel_format, Memory::Own { _pixels: own })
    }

    /// Over the first `stride * height` pixels of `region`'s mapping.
    ///
    /// # Safety
    /// Nothing writes the region's bytes while this lives but values on this
    /// thread over the same `Rc`, and no reference is formed over them.
    pub(crate) unsafe fn mapped(region: Rc<SharedMemory>, width: usize, height: usize, stride: usize, pixel_format: u32) -> Self {
        let (buf, len) = (region.as_ptr(), region.len());
        Self::over_raw(buf, len, width, height, stride, pixel_format, Memory::Mapped { _region: region })
    }
}

impl<'a> Framebuffer<'a> {
    /// Over the first `stride * height` pixels of `pixels`.
    pub fn over(pixels: &'a mut [u8], width: usize, height: usize, stride: usize, pixel_format: u32) -> Self {
        let (buf, len) = (pixels.as_mut_ptr(), pixels.len());
        Self::over_raw(buf, len, width, height, stride, pixel_format, Memory::Borrowed { _pixels: PhantomData })
    }

    /// The constructors' one body: `buf` is `len` bytes that `memory` keeps.
    fn over_raw(buf: *mut u8, len: usize, width: usize, height: usize, stride: usize, pixel_format: u32, memory: Memory<'a>) -> Self {
        assert!(
            stride >= width && pixels(stride, height).is_some_and(|bytes| bytes <= len),
            "Framebuffer: {width}x{height} at stride {stride} over {len} bytes"
        );
        Self {
            buf,
            len: stride * height * 4,
            width,
            height,
            stride,
            pixel_format: PixelFormat::from_raw(pixel_format),
            _memory: memory,
        }
    }

    /// Pixel `(x, y)`'s first byte, which the caller bounded inside the
    /// surface.
    fn at(&self, x: usize, y: usize) -> *const u8 {
        let offset = (y * self.stride + x) * 4;
        debug_assert!(offset < self.len);
        // SAFETY: inside the surface, as the caller bounded it.
        unsafe { self.buf.add(offset) }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn stride(&self) -> usize {
        self.stride
    }

    pub fn pixel_format_raw(&self) -> u32 {
        if self.pixel_format == PixelFormat::Rgb { 0 } else { 1 }
    }

    #[inline]
    fn encode_pixel(&self, color: Color) -> [u8; 4] {
        self.pixel_format.encode(color)
    }

    #[inline]
    pub fn get_pixel(&self, x: usize, y: usize) -> Color {
        if x < self.width && y < self.height {
            let offset = (y * self.stride + x) * 4;
            debug_assert!(offset + 4 <= self.len);
            // SAFETY: inside the surface, and a copy rather than a reference.
            let pixel = unsafe { self.buf.add(offset).cast::<[u8; 4]>().read_unaligned() };
            match self.pixel_format {
                PixelFormat::Rgb => Color { r: pixel[0], g: pixel[1], b: pixel[2] },
                PixelFormat::Bgr => Color { r: pixel[2], g: pixel[1], b: pixel[0] },
            }
        } else {
            Color { r: 0, g: 0, b: 0 }
        }
    }

    #[inline]
    pub fn put_pixel(&self, x: usize, y: usize, color: Color) {
        if x < self.width && y < self.height {
            let offset = (y * self.stride + x) * 4;
            debug_assert!(offset + 4 <= self.len);
            let pixel = self.encode_pixel(color);
            unsafe {
                ptr::copy_nonoverlapping(pixel.as_ptr(), self.buf.add(offset), 4);
            }
        }
    }

    /// Fill a row of pixels with a 4-byte pattern using doubling memcpy.
    unsafe fn fill_row(dst: *mut u8, pixel: &[u8; 4], count: usize) {
        if count == 0 {
            return;
        }
        ptr::copy_nonoverlapping(pixel.as_ptr(), dst, 4);
        let total_bytes = count * 4;
        let mut filled = 4usize;
        while filled < total_bytes {
            let chunk = filled.min(total_bytes - filled);
            ptr::copy_nonoverlapping(dst, dst.add(filled), chunk);
            filled += chunk;
        }
    }

    pub fn fill_rect(&self, x: usize, y: usize, w: usize, h: usize, color: Color) {
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        if x >= x_end || y >= y_end {
            return;
        }
        let actual_w = x_end - x;
        let rows = y_end - y;
        let row_bytes = actual_w * 4;
        let pixel = self.encode_pixel(color);

        unsafe {
            let first_row = self.buf.add((y * self.stride + x) * 4);
            debug_assert!((y * self.stride + x) * 4 + row_bytes <= self.len);
            Self::fill_row(first_row, &pixel, actual_w);
            for dy in 1..rows {
                let dst_offset = ((y + dy) * self.stride + x) * 4;
                debug_assert!(dst_offset + row_bytes <= self.len);
                let dst = self.buf.add(dst_offset);
                ptr::copy_nonoverlapping(first_row, dst, row_bytes);
            }
        }
    }

    /// Blit a buffer to a region of the framebuffer (row-by-row memcpy).
    /// `src_stride` is the width of the source buffer (may differ from `w` during resize).
    pub fn blit(&self, x: usize, y: usize, w: usize, h: usize, src_stride: usize, buffer: &[u8]) {
        self.blit_rows(x, y, w, h, src_stride, |at, dst, bytes| {
            let row = &buffer[at..at + bytes];
            // SAFETY: `dst` is `bytes` of this framebuffer, and `row` is a
            // reference, so it is not them.
            unsafe { ptr::copy_nonoverlapping(row.as_ptr(), dst, bytes) }
        });
    }

    /// [`Self::blit`] from bytes another party can write, such as a client's
    /// buffer: each row one copy out of them.
    pub fn blit_shared(&self, x: usize, y: usize, w: usize, h: usize, src_stride: usize, src: toyos::shm::ReadBytes<'_>) {
        self.blit_rows(x, y, w, h, src_stride, |at, dst, bytes| {
            // SAFETY: `dst` is `bytes` of this framebuffer, which composes into
            // memory of its own and never into a source it reads.
            unsafe { src.sub(at, bytes).read_to(dst) }
        });
    }

    /// Hand `row` each row the blit covers: where it starts in the source, its
    /// first byte here, and its length.
    fn blit_rows(&self, x: usize, y: usize, w: usize, h: usize, src_stride: usize, mut row: impl FnMut(usize, *mut u8, usize)) {
        let blit_w = w.min(self.width.saturating_sub(x));
        if blit_w == 0 {
            return;
        }
        let rows = h.min(self.height.saturating_sub(y));
        let copy_bytes = blit_w * 4;
        let src_row_bytes = src_stride * 4;
        for dy in 0..rows {
            let dst_offset = ((y + dy) * self.stride + x) * 4;
            debug_assert!(dst_offset + copy_bytes <= self.len);
            // SAFETY: inside the framebuffer, as its constructor checked.
            row(dy * src_row_bytes, unsafe { self.buf.add(dst_offset) }, copy_bytes);
        }
    }

    pub fn clear(&self, color: Color) {
        self.fill_rect(0, 0, self.width, self.height, color);
    }
}

impl font::Canvas for Framebuffer<'_> {
    fn put_pixel(&self, x: usize, y: usize, color: Color) {
        self.put_pixel(x, y, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color = Color { r: 0xff, g: 0, b: 0 };

    #[test]
    fn a_borrowed_surface_draws_into_the_bytes_it_borrowed_and_no_others() {
        let mut bytes = vec![0xEEu8; 3 * 2 * 4 + 4];
        let surface = Framebuffer::over(&mut bytes, 2, 2, 3, 0);
        surface.fill_rect(1, 0, 1, 2, RED);
        drop(surface);
        let pixel = |x: usize, y: usize| &bytes[(y * 3 + x) * 4..][..4];
        assert_eq!(pixel(1, 0), [0xff, 0, 0, 0]);
        assert_eq!(pixel(1, 1), [0xff, 0, 0, 0]);
        assert_eq!(pixel(0, 0), [0xEE; 4]);
        assert_eq!(pixel(2, 1), [0xEE; 4], "the stride's padding was drawn");
        assert_eq!(&bytes[3 * 2 * 4..], [0xEE; 4], "past the surface was drawn");
    }

    #[test]
    fn an_owned_surface_starts_black() {
        let surface = Framebuffer::new(2, 1, 1);
        assert!(surface.get_pixel(1, 0) == Color { r: 0, g: 0, b: 0 });
        surface.put_pixel(0, 0, RED);
        assert!(surface.get_pixel(0, 0) == RED);
    }

    #[test]
    #[should_panic(expected = "over 23 bytes")]
    fn a_surface_over_too_few_bytes_is_refused() {
        Framebuffer::over(&mut [0; 23], 2, 3, 2, 0);
    }

    #[test]
    #[should_panic(expected = "at stride 1")]
    fn a_stride_narrower_than_a_row_is_refused() {
        Framebuffer::over(&mut [0; 64], 2, 2, 1, 0);
    }

    #[test]
    #[should_panic(expected = "Framebuffer:")]
    fn a_size_whose_bytes_overflow_is_refused() {
        Framebuffer::over(&mut [0; 64], usize::MAX, 2, usize::MAX, 0);
    }
}
