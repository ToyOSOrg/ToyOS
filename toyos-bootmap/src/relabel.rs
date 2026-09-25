//! The map the kernel is handed says what an extent holds where the firmware's
//! map cannot: the loader allocates such pages as a type firmware minted
//! itself, and [`relabel`] gives them their own type only in the copy taken
//! after `ExitBootServices`, so firmware never walks a descriptor of a type it
//! did not define.

/// `[start, end)` of UEFI memory type `ty`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Extent {
    pub start: u64,
    pub end: u64,
    pub ty: u32,
}

/// `extent`, with the part of it inside `claim` given `claim.ty` where `extent`
/// is of type `from`: up to three non-empty extents in address order, together
/// covering exactly `extent`.
///
/// An `extent` of another type is returned whole, so a claim the firmware
/// placed over memory it did not give out as `from` stays unmarked and the
/// kernel's own check of the claim refuses it.
pub fn relabel(extent: Extent, from: u32, claim: Extent) -> [Option<Extent>; 3] {
    let start = extent.start.max(claim.start);
    let end = extent.end.min(claim.end);
    if extent.ty != from || start >= end {
        return [Some(extent), None, None];
    }
    [
        (extent.start < start).then_some(Extent { end: start, ..extent }),
        Some(Extent { start, end, ty: claim.ty }),
        (end < extent.end).then_some(Extent { start: end, ..extent }),
    ]
}
