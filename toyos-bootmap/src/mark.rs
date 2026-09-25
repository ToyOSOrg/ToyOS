//! The handoff's marks: white squares on the scanout, painted where no line of
//! text can be, one per step between the loader's last line and the kernel's
//! first record.
//!
//! The loader's console dies with boot services and the kernel's panel arms
//! only after `pat::init` and a walk of the boot map, so a machine that stops
//! in that span otherwise leaves the loader's last line and nothing that says
//! which step stopped it. The squares stand in a row at the top right, in
//! [`Step`] order from the left; the count on the screen is the steps reached.
//!
//! White is `0x00FF_FFFF` in both scanout formats the loader hands over, so a
//! square reads the same on an RGB panel and a BGR one.

/// A step of the handoff, in the order the machine passes them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// `ExitBootServices` returned: the firmware let go.
    BootServicesExited = 0,
    /// The loader runs on its own boot map, the one `mov cr3` just loaded.
    BootMapLive = 1,
    /// The kernel's first statement, before anything of its own can fault.
    KernelEntered = 2,
}

impl Step {
    pub const ALL: [Step; 3] = [Step::BootServicesExited, Step::BootMapLive, Step::KernelEntered];
}

/// One square's side, in pixels.
pub const SIDE: u32 = 32;

/// Between two squares, and between the row and the panel's top and right edges.
pub const GAP: u32 = 16;

/// The one colour every square is painted in.
pub const WHITE: u32 = 0x00FF_FFFF;

/// A scanout as the loader hands it over: `stride` pixels a row, 4 bytes a pixel.
#[derive(Clone, Copy, Debug)]
pub struct Scanout {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub bytes: u64,
}

/// The byte offsets into the scanout of every pixel `step`'s square covers, or
/// `None` for a scanout that cannot hold the whole row, which gets no square at
/// all: every offset returned is inside `scanout.bytes`.
pub fn square(step: Step, scanout: Scanout) -> Option<impl Iterator<Item = u64>> {
    let row = Step::ALL.len() as u32 * (SIDE + GAP) + GAP;
    if scanout.width < row || scanout.height < SIDE + 2 * GAP || scanout.stride < scanout.width {
        return None;
    }
    // Checked: the loader calls this past `ExitBootServices`, where a panic has
    // nowhere to go.
    let whole = u64::from(scanout.stride).checked_mul(u64::from(scanout.height))?.checked_mul(4)?;
    if scanout.bytes < whole {
        return None;
    }
    let left = scanout.width - row + GAP + step as u32 * (SIDE + GAP);
    let stride = u64::from(scanout.stride);
    Some((GAP..GAP + SIDE).flat_map(move |y| {
        (left..left + SIDE).map(move |x| (u64::from(y) * stride + u64::from(x)) * 4)
    }))
}
