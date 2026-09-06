//! Where a boot stopped, on a machine with no channel to say so.
//!
//! Between `ExitBootServices` and the kernel's first log record there is no
//! channel at all: UEFI's console is a boot service, the panic console is not
//! armed, and a machine with no serial port has nothing else. A crumb is one
//! block of solid colour written straight into the framebuffer at a step in
//! that window; the blocks run left to right along the bottom edge of the
//! panel, so the rightmost one is the last step reached.
//!
//! They sit in the strip below the console's last cell row, which a
//! cell-repainting console never writes: a boot that finishes leaves its crumbs
//! on the glass beside its log, so the panel answers for the whole boot and not
//! only for the part that failed.
//!
//! Pure but for [`Pen::paint`]'s store: the bootloader and the kernel are built
//! for different targets and hold the same framebuffer at different addresses,
//! so each supplies its own pointer and this decides everything else.

#![no_std]

/// The boot parameter that arms every crumb. `kernel/src/params.rs` declares
/// the kernel's row for it; this is the only spelling either binary reads.
pub const PARAM: &str = "early-breadcrumbs";

/// Whether `cmdline` names [`PARAM`], read with the tokeniser the kernel's own
/// parameter table is read with.
pub fn armed(cmdline: &str) -> bool {
    toyos_abi::boot::actuators(cmdline).any(|token| token == PARAM)
}

/// Every step a crumb marks, left to right. The bootloader paints the first
/// five, all of them past `ExitBootServices`; the kernel paints the rest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// `ExitBootServices` returned, so firmware is gone and the console with it.
    Exited,
    /// The UEFI memory map is copied into the form `KernelArgs` hands on.
    Mapped,
    /// The boot page tables are built.
    Paged,
    /// `CR3` holds them: from here the panel is reached through the boot map, not through firmware's.
    Cr3,
    /// The kernel image fits the boot map; the entry call is the next instruction.
    Jumping,
    /// `kernel_main` has its own stack and has read the boot parameter.
    Entered,
    /// `panic_console::arm` returned.
    Armed,
    /// `serial::init` returned.
    Serial,
    /// The boot parameter, the actuators and the root filesystem's name are parsed.
    Params,
}

/// Every step, so a reader of the panel can count what is missing.
pub const STEPS: [Step; 9] = [
    Step::Exited,
    Step::Mapped,
    Step::Paged,
    Step::Cr3,
    Step::Jumping,
    Step::Entered,
    Step::Armed,
    Step::Serial,
    Step::Params,
];

/// How tall the strip is. Eight rows is the panel the T14 has (1080) less the
/// 67 sixteen-pixel cell rows the console paints on it.
pub const STRIP_PX: u32 = 8;

/// The rows `[top, bottom)` every crumb shares, or `None` on a panel with no room for the strip.
pub const fn strip(height: u32) -> Option<(u32, u32)> {
    let Some(top) = height.checked_sub(STRIP_PX) else { return None };
    Some((top, height))
}

/// The columns `[left, right)` this step paints, or `None` on a panel too
/// narrow to give every step a block of its own.
pub const fn block(step: Step, width: u32) -> Option<(u32, u32)> {
    let each = width / STEPS.len() as u32;
    if each == 0 {
        return None;
    }
    let left = step as u32 * each;
    Some((left, left + each))
}

/// A block's colour, cycling through three so that no two neighbours share one:
/// a block that painted alone is named by where it sits, and a run of them is
/// counted by the edges between them.
pub const fn rgb(step: Step) -> (u8, u8, u8) {
    match step as u32 % 3 {
        0 => (255, 0, 0),
        1 => (0, 255, 0),
        _ => (0, 128, 255),
    }
}

/// The 32-bit pixel word for a display whose `format` is `KernelArgs::gop_pixel_format`: 0 is RGB, 1 is BGR.
pub const fn pixel(step: Step, format: u32) -> u32 {
    let (r, g, b) = rgb(step);
    let (r, g, b) = (r as u32, g as u32, b as u32);
    if format == 0 {
        r | (g << 8) | (b << 16)
    } else {
        b | (g << 8) | (r << 16)
    }
}

/// The framebuffer crumbs are painted on, as its holder can reach it *now*:
/// the loader's address before the CR3 switch is firmware's, the kernel's is
/// the boot map's, and neither outlives the mapping it was built from.
#[derive(Clone, Copy)]
pub struct Pen {
    fb: *mut u32,
    bytes: u64,
    stride_px: u32,
    width: u32,
    height: u32,
    format: u32,
}

impl Pen {
    /// # Safety
    /// `fb` must point at a mapped, writable framebuffer of at least `bytes`
    /// bytes, laid out as `stride_px` 32-bit pixels per row, and must stay
    /// mapped for as long as this `Pen` is painted with.
    pub const unsafe fn new(
        fb: *mut u32,
        bytes: u64,
        stride_px: u32,
        width: u32,
        height: u32,
        format: u32,
    ) -> Pen {
        Pen { fb, bytes, stride_px, width, height, format }
    }

    /// Paint this step's block, stopping at whatever `bytes` the descriptor
    /// declared: firmware's numbers decide the extent, and a block past it is
    /// dropped rather than wrapped onto the rows above.
    pub fn paint(&self, step: Step) {
        let (Some((top, bottom)), Some((left, right))) =
            (strip(self.height), block(step, self.width))
        else {
            return;
        };
        let word = pixel(step, self.format);
        let pixels = self.bytes / 4;
        for row in top..bottom {
            let start = row as u64 * self.stride_px as u64;
            for index in start + left as u64..start + right as u64 {
                if index >= pixels {
                    return;
                }
                // SAFETY: `index < self.bytes / 4`, so this 4-byte store lands
                // inside the framebuffer `new`'s caller promised is mapped.
                unsafe { self.fb.add(index as usize).write_volatile(word) };
            }
        }
    }
}

/// Paint `step` where a pen exists. A boot that armed no crumbs, and a machine
/// that publishes no framebuffer, hold `None` and paint nothing.
pub fn crumb(pen: &Option<Pen>, step: Step) {
    if let Some(pen) = pen {
        pen.paint(step);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec;

    /// The panel `Profile::Metal` advertises and the T14 has.
    const T14: (u32, u32) = (1920, 1080);

    /// The console's cell height, which is what leaves the strip free.
    const CELL_PX: u32 = 16;

    #[test]
    fn the_strip_is_what_the_console_leaves_of_the_t14_panel() {
        let (top, bottom) = strip(T14.1).expect("the T14 panel holds a strip");
        assert_eq!(bottom, T14.1);
        assert_eq!(top, T14.1 / CELL_PX * CELL_PX, "the strip starts at the last cell row's end");
        assert_eq!(strip(STRIP_PX - 1), None);
    }

    #[test]
    fn the_blocks_run_left_to_right_without_touching() {
        let mut previous_right = 0;
        for step in STEPS {
            let (left, right) = block(step, T14.0).expect("the T14 panel holds every block");
            assert_eq!(left, previous_right, "{step:?} does not start where the one before ended");
            assert!(right > left, "{step:?} is empty");
            previous_right = right;
        }
        assert!(previous_right <= T14.0, "the run stays on the panel");
    }

    #[test]
    fn a_panel_too_narrow_for_a_block_paints_none_of_it() {
        assert_eq!(block(Step::Exited, STEPS.len() as u32 - 1), None);
        assert_eq!(block(Step::Exited, STEPS.len() as u32), Some((0, 1)));
        assert_eq!(block(Step::Params, STEPS.len() as u32), Some((8, 9)));
    }

    #[test]
    fn neighbouring_blocks_never_share_a_colour() {
        for pair in STEPS.windows(2) {
            assert_ne!(rgb(pair[0]), rgb(pair[1]), "{:?} and {:?}", pair[0], pair[1]);
        }
    }

    #[test]
    fn the_format_swaps_the_red_and_blue_channels() {
        let rgb_word = pixel(Step::Exited, 0);
        let bgr_word = pixel(Step::Exited, 1);
        assert_ne!(rgb_word, bgr_word, "a red block is the case that tells the two apart");
        assert_eq!(rgb_word & 0xff, (bgr_word >> 16) & 0xff);
        assert_eq!((rgb_word >> 16) & 0xff, bgr_word & 0xff);
        assert_eq!((rgb_word >> 8) & 0xff, (bgr_word >> 8) & 0xff);
    }

    #[test]
    fn the_parameter_is_read_as_a_whole_token() {
        assert!(armed(PARAM));
        assert!(armed("root=8b3d,early-breadcrumbs"));
        assert!(armed("early-breadcrumbs,watchdog"));
        assert!(!armed("root=8b3d,watchdog"));
        assert!(!armed(""));
        assert!(!armed("early-breadcrumbsx"));
        assert!(!armed("xearly-breadcrumbs"));
        // `root=` is the one token `actuators` drops, so a filesystem named
        // after the parameter does not arm it.
        assert!(!armed("root=early-breadcrumbs"));
    }

    #[test]
    fn a_pen_paints_its_own_block_and_nothing_else() {
        let (width, height) = (STEPS.len() as u32 * 3, STRIP_PX + 2);
        let stride = width as usize + 2;
        let mut fb = vec![0u32; stride * height as usize];
        // SAFETY: `fb` is a live allocation of exactly this many pixels and
        // outlives every `paint` below.
        let pen = unsafe {
            Pen::new(fb.as_mut_ptr(), (fb.len() * 4) as u64, stride as u32, width, height, 1)
        };
        pen.paint(Step::Cr3);
        let (top, bottom) = strip(height).expect("this panel holds one");
        let (left, right) = block(Step::Cr3, width).expect("this panel holds one");
        for row in 0..height {
            for column in 0..stride as u32 {
                let inside = (top..bottom).contains(&row) && (left..right).contains(&column);
                let want = if inside { pixel(Step::Cr3, 1) } else { 0 };
                assert_eq!(fb[row as usize * stride + column as usize], want, "{row},{column}");
            }
        }
    }

    #[test]
    fn a_pen_stops_at_the_buffer_its_descriptor_declared() {
        let (width, height) = (STEPS.len() as u32 * 3, STRIP_PX + 2);
        let stride = width as usize;
        let (top, _) = strip(height).expect("this panel holds one");
        let (last_left, _) = block(Step::Params, width).expect("this panel holds one");
        // A descriptor whose buffer ends exactly where the last block begins.
        let short = (top as usize * stride + last_left as usize) as u64 * 4;
        let mut fb = vec![0u32; stride * height as usize];
        // SAFETY: `short` is shorter than a live allocation, so every store the
        // pen makes is inside it.
        let pen = unsafe { Pen::new(fb.as_mut_ptr(), short, stride as u32, width, height, 0) };

        pen.paint(Step::Params);
        assert!(fb.iter().all(|word| *word == 0), "a block past the declared buffer painted");
        // The control: the block the same buffer does reach is painted, so the
        // assertion above is about the bound and not about a pen that never draws.
        pen.paint(Step::Exited);
        assert_eq!(fb[top as usize * stride], pixel(Step::Exited, 0));
    }
}
