//! What firmware-reserved memory regions (VT-d RMRRs) do to the functions
//! they name, by the owner's ruling of 2026-10-04 ("Display and USB only"):
//! a display or USB controller whose reserved memory is its alone keeps it,
//! mapped into its driver's own domain; every other function a region names is
//! refused a claim.
//!
//! Judged over the whole DMAR at once, not one structure at a time: a function's
//! memory is its alone only where every region overlapping any of its own
//! names it and nothing else. Identical and overlapping regions of one
//! function merge, so each address is kept once.

use alloc::vec::Vec;

/// PCI class codes, as config space's dword at 08h holds them above its
/// revision: base class 03h is a display controller, 0C03h a USB one.
const DISPLAY: u32 = 0x03;
const USB: u32 = 0x0C03;

const PAGE: u64 = 0x1000;
const LEAF: u64 = 0x20_0000;

/// One device scope of a region, as the walk resolved it. A function is its
/// requester id, `bus << 8 | device << 3 | function`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// A PCI endpoint at the end of a path that was followed.
    Endpoint(u16),
    /// A bridge that was followed, and every function enumerated below it.
    SubHierarchy(Vec<u16>),
    /// Anything that names a device without naming it here: a path through a
    /// bridge that could not be followed, another scope kind (an ACPI
    /// namespace device can master), or a malformed entry and the rest of the
    /// list after it.
    Unknown,
}

/// One RMRR: `start..end`, and its scopes.
#[derive(Clone, Debug)]
pub struct Region {
    pub start: u64,
    pub end: u64,
    pub scopes: Vec<Scope>,
}

impl Region {
    fn overlaps(&self, start: u64, end: u64) -> bool {
        self.start < end && start < self.end
    }

    /// Every function it names, and whether it names anything else besides.
    fn named(&self) -> (Vec<u16>, bool) {
        let mut named = Vec::new();
        let mut other = false;
        for scope in &self.scopes {
            match scope {
                Scope::Endpoint(id) => named.push(*id),
                Scope::SubHierarchy(below) => {
                    named.extend(below);
                    other = true;
                }
                Scope::Unknown => other = true,
            }
        }
        named.sort_unstable();
        named.dedup();
        (named, other)
    }
}

/// One function's verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// These disjoint ranges, kept in the function's own domain.
    Kept(Vec<(u64, u64)>),
    /// No claim may have the function, and why.
    Refused(&'static str),
}

/// Every function the regions name, once each, with its verdict. `class`
/// answers config space's class code (dword 08h shifted right by 8) for a
/// function, or `None` for one nobody enumerated.
pub fn classify(regions: &[Region], class: impl Fn(u16) -> Option<u32>) -> Vec<(u16, Verdict)> {
    let named: Vec<(&Region, (Vec<u16>, bool))> = regions.iter().map(|r| (r, r.named())).collect();
    let mut functions: Vec<u16> = named.iter().flat_map(|(_, (ids, _))| ids.iter().copied()).collect();
    functions.sort_unstable();
    functions.dedup();
    functions
        .into_iter()
        .map(|id| {
            let own: Vec<&(&Region, (Vec<u16>, bool))> = named.iter().filter(|(_, (ids, _))| ids.contains(&id)).collect();
            let alone = named
                .iter()
                .filter(|(r, _)| own.iter().any(|(o, _)| r.overlaps(o.start, o.end)))
                .all(|(_, (ids, other))| !other && ids[..] == [id]);
            let own: Vec<&Region> = own.iter().map(|(r, _)| *r).collect();
            let controller = class(id).is_some_and(|c| c >> 16 == DISPLAY || c >> 8 == USB);
            let whole = own.iter().all(|r| r.start.is_multiple_of(PAGE) && r.end.is_multiple_of(PAGE) && r.start < r.end);
            let verdict = if !alone {
                Verdict::Refused("firmware reserves memory for it that another device shares")
            } else if !controller {
                Verdict::Refused("firmware reserves memory for it, and it is neither a display nor a USB controller")
            } else if !whole {
                Verdict::Refused("firmware reserves memory for it that is not whole 4 KiB pages")
            } else {
                Verdict::Kept(merged(own.iter().map(|r| (r.start, r.end))))
            };
            (id, verdict)
        })
        .collect()
}

/// Whether `start..end` lies wholly inside the union of `ranges`.
pub fn covered(ranges: &[(u64, u64)], start: u64, end: u64) -> bool {
    merged(ranges.iter().copied()).iter().any(|&(from, to)| from <= start && end <= to)
}

/// The union of `ranges`, as disjoint ranges in order.
fn merged(ranges: impl Iterator<Item = (u64, u64)>) -> Vec<(u64, u64)> {
    let mut ranges: Vec<(u64, u64)> = ranges.collect();
    ranges.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (start, end) in ranges {
        match out.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

/// A leaf the tables map a kept range with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leaf {
    /// A 2 MiB leaf at this address.
    Large(u64),
    /// A 4 KiB page at this address.
    Page(u64),
}

/// `start..end`, whole 4 KiB pages, as 2 MiB leaves wherever one fits whole
/// and 4 KiB pages elsewhere.
pub fn leaves(start: u64, end: u64) -> impl Iterator<Item = Leaf> {
    let mut at = start;
    core::iter::from_fn(move || {
        if at >= end {
            return None;
        }
        let leaf = if at.is_multiple_of(LEAF) && at + LEAF <= end {
            at += LEAF;
            Leaf::Large(at - LEAF)
        } else {
            at += PAGE;
            Leaf::Page(at - PAGE)
        };
        Some(leaf)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// The T14's: `0x9c000000..0xa07fffff`, naming the iGPU `00:02.0` (class 030000h) alone.
    const T14: (u64, u64) = (0x9c00_0000, 0xa080_0000);
    const IGPU: u16 = 0x0010;
    const XHCI: u16 = 0x00A0;
    const NIC: u16 = 0x00FE;

    fn class(id: u16) -> Option<u32> {
        match id {
            IGPU => Some(0x03_00_00),
            XHCI => Some(0x0C_03_30),
            NIC => Some(0x02_00_00),
            _ => None,
        }
    }

    fn region(start: u64, end: u64, scopes: Vec<Scope>) -> Region {
        Region { start, end, scopes }
    }

    fn refused(found: &[(u16, Verdict)], id: u16) -> bool {
        found.iter().any(|(named, v)| *named == id && matches!(v, Verdict::Refused(_)))
    }

    #[test]
    fn one_endpoint_display_controller_keeps_its_region() {
        let found = classify(&[region(T14.0, T14.1, vec![Scope::Endpoint(IGPU)])], class);
        assert_eq!(found, [(IGPU, Verdict::Kept(vec![T14]))]);
    }

    #[test]
    fn a_usb_controller_keeps_a_small_region_off_2_mib() {
        let found = classify(&[region(0x7b80_3000, 0x7b80_5000, vec![Scope::Endpoint(XHCI)])], class);
        assert_eq!(found, [(XHCI, Verdict::Kept(vec![(0x7b80_3000, 0x7b80_5000)]))]);
    }

    #[test]
    fn two_endpoints_in_one_region_are_both_refused() {
        let found = classify(&[region(T14.0, T14.1, vec![Scope::Endpoint(IGPU), Scope::Endpoint(XHCI)])], class);
        assert!(refused(&found, IGPU) && refused(&found, XHCI));
    }

    #[test]
    fn a_sub_hierarchy_refuses_every_function_below_it() {
        let found = classify(&[region(T14.0, T14.1, vec![Scope::SubHierarchy(vec![XHCI, NIC])])], class);
        assert!(refused(&found, XHCI) && refused(&found, NIC));
    }

    #[test]
    fn an_unknown_scope_beside_an_endpoint_refuses_it() {
        // An unfollowable path, an ACPI namespace device (kind 5) and a
        // malformed list all resolve to `Unknown`.
        let found = classify(&[region(T14.0, T14.1, vec![Scope::Endpoint(IGPU), Scope::Unknown])], class);
        assert!(refused(&found, IGPU));
    }

    #[test]
    fn a_function_neither_display_nor_usb_is_refused() {
        let found = classify(&[region(T14.0, T14.1, vec![Scope::Endpoint(NIC)])], class);
        assert!(refused(&found, NIC));
        let found = classify(&[region(T14.0, T14.1, vec![Scope::Endpoint(0x0123)])], class);
        assert!(refused(&found, 0x0123));
    }

    #[test]
    fn a_region_not_whole_pages_is_refused() {
        let found = classify(&[region(T14.0 + 0x800, T14.1, vec![Scope::Endpoint(IGPU)])], class);
        assert!(refused(&found, IGPU));
    }

    #[test]
    fn overlapping_regions_of_two_functions_refuse_both() {
        let regions = [
            region(T14.0, T14.1, vec![Scope::Endpoint(IGPU)]),
            region(T14.1 - 0x1000, T14.1 + 0x1000, vec![Scope::Endpoint(XHCI)]),
        ];
        let found = classify(&regions, class);
        assert!(refused(&found, IGPU) && refused(&found, XHCI));
    }

    #[test]
    fn duplicate_and_overlapping_regions_of_one_function_are_kept_once() {
        let regions = [
            region(T14.0, T14.1, vec![Scope::Endpoint(IGPU)]),
            region(T14.0, T14.1, vec![Scope::Endpoint(IGPU), Scope::Endpoint(IGPU)]),
            region(T14.1 - 0x20_0000, T14.1 + 0x1000, vec![Scope::Endpoint(IGPU)]),
        ];
        assert_eq!(classify(&regions, class), [(IGPU, Verdict::Kept(vec![(T14.0, T14.1 + 0x1000)]))]);
    }

    #[test]
    fn every_kept_range_is_covered_by_the_rmrrs_it_came_from_and_by_no_single_one() {
        let regions = [
            region(T14.0, T14.1, vec![Scope::Endpoint(IGPU)]),
            region(T14.1 - 0x20_0000, T14.1 + 0x1000, vec![Scope::Endpoint(IGPU)]),
        ];
        let sources: Vec<(u64, u64)> = regions.iter().map(|r| (r.start, r.end)).collect();
        let [(_, Verdict::Kept(kept))] = &classify(&regions, class)[..] else { panic!("kept") };
        for &(start, end) in kept {
            assert!(covered(&sources, start, end));
            // The merged range spans both: what a per-region check refused.
            assert!(!sources.iter().any(|&(from, to)| from <= start && end <= to));
        }
        assert!(!covered(&sources, T14.0, T14.1 + 0x2000));
    }

    #[test]
    fn a_range_off_2_mib_takes_pages_at_its_ends_and_leaves_between() {
        let found: Vec<Leaf> = leaves(0x1F_E000, 0x40_2000).collect();
        assert_eq!(
            found,
            [Leaf::Page(0x1F_E000), Leaf::Page(0x1F_F000), Leaf::Large(0x20_0000), Leaf::Page(0x40_0000), Leaf::Page(0x40_1000)]
        );
        assert_eq!(leaves(T14.0, T14.1).count(), ((T14.1 - T14.0) / LEAF) as usize);
    }
}
