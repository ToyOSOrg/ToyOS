//! The 4 KiB pages of a BAR where firmware put it, which is all a process
//! that drives the function is given of it.
//!
//! **A page holding another function's registers is not handed over**: its
//! holder would drive that function too. A BAR whose first byte is not a page's
//! is refused as well, because a mapping begins at a page and a holder is told
//! a BAR's size and no offset into one.

use core::fmt;

/// The page a process is given a device's registers in.
pub const PAGE: u64 = 4096;

/// `bytes` from `at`, both whole pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pages {
    pub at: u64,
    pub bytes: u64,
}

impl Pages {
    pub fn end(self) -> u64 {
        self.at + self.bytes
    }
}

/// Why a BAR's pages are not handed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// It starts `offset` bytes into its page.
    OffPage { offset: u64 },
    /// Its last page would end past the top of the address space.
    Wraps,
    /// Requester `with` decodes `start..end`, inside these pages.
    Shared { with: u16, start: u64, end: u64 },
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OffPage { offset } => write!(
                f,
                "it starts {offset:#x} bytes into its 4 KiB page, and a holder is given whole pages \
                 and no offset into one"
            ),
            Self::Wraps => write!(f, "its last 4 KiB page ends past the top of the address space"),
            Self::Shared { with, start, end } => write!(
                f,
                "requester {with:#06x} decodes {start:#x}..{end:#x} inside its 4 KiB pages, and \
                 their holder would drive that function too"
            ),
        }
    }
}

/// The pages a `bytes`-long BAR at `base` covers, or why they are not handed
/// over. `others` is every extent another function decodes, as `(requester,
/// start, end)`; `bytes` is at least one.
pub fn of_bar(base: u64, bytes: u64, others: impl IntoIterator<Item = (u16, u64, u64)>) -> Result<Pages, Refused> {
    assert!(bytes > 0, "pages: a BAR of no bytes");
    let offset = base % PAGE;
    if offset != 0 {
        return Err(Refused::OffPage { offset });
    }
    let pages = Pages { at: base, bytes: bytes.div_ceil(PAGE) * PAGE };
    pages.at.checked_add(pages.bytes).ok_or(Refused::Wraps)?;
    match others.into_iter().find(|&(_, start, end)| start < pages.end() && pages.at < end) {
        Some((with, start, end)) => Err(Refused::Shared { with, start, end }),
        None => Ok(pages),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The AX210 on the T14, as S1 read it: BAR 0, 16 KiB at 0xbc300000.
    const AX210: (u64, u64) = (0xbc30_0000, 0x4000);

    #[test]
    fn a_bar_alone_in_its_pages_is_handed_over_whole_and_in_place() {
        let (base, bytes) = AX210;
        assert_eq!(of_bar(base, bytes, []), Ok(Pages { at: base, bytes }));
        // Neighbours that end where it starts and start where it ends touch none of its pages.
        let neighbours = [(0x0100, base - 0x1000, base), (0x0200, base + bytes, base + 2 * bytes)];
        assert_eq!(of_bar(base, bytes, neighbours), Ok(Pages { at: base, bytes }));
    }

    #[test]
    fn a_bar_smaller_than_a_page_is_given_the_page_it_starts() {
        // The T14's SMBus controller's 256 bytes, at the start of a page.
        assert_eq!(of_bar(0x0060_3dbd_0000, 0x100, []), Ok(Pages { at: 0x0060_3dbd_0000, bytes: PAGE }));
    }

    #[test]
    fn a_page_another_function_decodes_in_is_refused_by_name() {
        // Another function's 16 bytes in the rest of a sub-page BAR's page.
        let other = (0x00f8, 0xd040_0100, 0xd040_0110);
        assert_eq!(
            of_bar(0xd040_0000, 0x100, [other]),
            Err(Refused::Shared { with: 0x00f8, start: 0xd040_0100, end: 0xd040_0110 })
        );
        // One overlapping the BAR itself, and one reaching into its last page from above.
        let over = (0x0300, 0xd040_1000, 0xd040_1004);
        assert!(matches!(of_bar(0xd040_0000, 0x2000, [over]), Err(Refused::Shared { with: 0x0300, .. })));
        let below = (0x0400, 0xd03f_f000, 0xd040_0001);
        assert!(matches!(of_bar(0xd040_0000, 0x2000, [below]), Err(Refused::Shared { with: 0x0400, .. })));
    }

    #[test]
    fn a_bar_inside_a_page_is_refused_by_name() {
        assert_eq!(of_bar(0xd040_0080, 0x80, []), Err(Refused::OffPage { offset: 0x80 }));
    }

    #[test]
    fn a_bar_whose_last_page_wraps_is_refused_by_name() {
        assert_eq!(of_bar(u64::MAX - 0xfff, 0x2000, []), Err(Refused::Wraps));
    }
}
