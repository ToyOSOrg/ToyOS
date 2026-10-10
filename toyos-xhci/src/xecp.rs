//! The controller's extended capability list (xHCI 1.2 §7), and what the USB
//! Legacy Support capability in it decides (§4.22.1, §7.1).
//!
//! The list is the controller's and its firmware's, so it is untrusted: the
//! walk reads through a caller's bounded read, refuses a link that leaves the
//! register window, and stops after [`MAX_CAPS`] links whatever the list says.

/// Capability ID 1, USB Legacy Support (§7.1.1), and ID 2, Supported Protocol
/// (§7.2).
pub const CAP_ID_LEGACY: u8 = 1;
pub const CAP_ID_PROTOCOL: u8 = 2;

/// USBLEGSUP bit 16, HC BIOS Owned Semaphore, and bit 24, HC OS Owned
/// Semaphore (§7.1.1).
pub const LEGSUP_BIOS_OWNED: u32 = 1 << 16;
pub const LEGSUP_OS_OWNED: u32 = 1 << 24;

/// USBLEGCTLSTS, one dword past USBLEGSUP (§7.1.2).
pub const LEGCTLSTS: u64 = 4;

/// USBLEGCTLSTS's SMI enables: USB SMI, Host System Error, OS Ownership,
/// PCI Command and BAR (§7.1.2, bits 0, 4, 13, 14, 15).
pub const SMI_ENABLES: u32 = (1 << 0) | (1 << 4) | (1 << 13) | (1 << 14) | (1 << 15);

/// Its write-1-to-clear SMI status bits: OS Ownership Change, PCI Command and
/// BAR (bits 29, 30, 31).
pub const SMI_STATUS: u32 = (1 << 29) | (1 << 30) | (1 << 31);

/// The most links a walk follows, independent of what the list says.
pub const MAX_CAPS: usize = 64;

/// Why a walk stopped before the list's own end.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WalkError {
    /// A capability header outside the register window, at this offset.
    OutOfWindow(u64),
    /// More links than [`MAX_CAPS`].
    TooMany,
}

/// Visit every capability whose ID is `id`, in list order.
///
/// `xecp_dwords` is HCCPARAMS1's xECP field (bits 31:16), in dwords from the
/// register window's base; `read` answers `None` for any offset outside the
/// window. An ID may appear more than once — every Supported Protocol range is
/// its own capability.
pub fn for_each(
    read: &dyn Fn(u64) -> Option<u32>,
    xecp_dwords: u32,
    id: u8,
    visit: &mut dyn FnMut(u64),
) -> Result<(), WalkError> {
    if xecp_dwords == 0 {
        return Ok(());
    }
    let mut offset = u64::from(xecp_dwords) * 4;
    for _ in 0..MAX_CAPS {
        let header = read(offset).ok_or(WalkError::OutOfWindow(offset))?;
        if header as u8 == id {
            visit(offset);
        }
        let next = (header >> 8) & 0xFF;
        if next == 0 {
            return Ok(());
        }
        // 1..=255 dwords a step, so the offset strictly grows and cannot wrap.
        offset += u64::from(next) * 4;
    }
    Err(WalkError::TooMany)
}

/// The first capability whose ID is `id`, or `None` for a list without one.
pub fn find(read: &dyn Fn(u64) -> Option<u32>, xecp_dwords: u32, id: u8) -> Result<Option<u64>, WalkError> {
    let mut found = None;
    for_each(read, xecp_dwords, id, &mut |at| {
        found = found.or(Some(at));
    })?;
    Ok(found)
}

/// Who held the controller when the OS asked for it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Handoff {
    /// Firmware never set its semaphore.
    NeverClaimed,
    /// Firmware held it and cleared its semaphore once asked.
    Released,
    /// Firmware still holds it after the bound the caller gave it.
    Kept,
}

/// What a USBLEGSUP read `before` the OS set its semaphore, and the read
/// `now`, say of the handoff.
pub fn handoff(before: u32, now: u32) -> Handoff {
    if now & LEGSUP_BIOS_OWNED != 0 {
        Handoff::Kept
    } else if before & LEGSUP_BIOS_OWNED != 0 {
        Handoff::Released
    } else {
        Handoff::NeverClaimed
    }
}

/// The USBLEGCTLSTS write that turns every SMI the controller can raise off
/// and clears every SMI it has latched, from what it reads.
pub fn smis_off(ctlsts: u32) -> u32 {
    (ctlsts & !SMI_ENABLES) | SMI_STATUS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sixteen-dword window; a read at or past byte 64 is outside it.
    fn window(cells: &[u32; 16]) -> impl Fn(u64) -> Option<u32> + '_ {
        move |at: u64| (at % 4 == 0 && at + 4 <= 64).then(|| cells[(at / 4) as usize])
    }

    fn header(id: u8, next: u8) -> u32 {
        u32::from(id) | (u32::from(next) << 8)
    }

    #[test]
    fn the_walk_finds_each_capability_of_an_id_in_order() {
        let mut cells = [0u32; 16];
        cells[4] = header(CAP_ID_PROTOCOL, 2);
        cells[6] = header(CAP_ID_LEGACY, 4);
        cells[10] = header(CAP_ID_PROTOCOL, 0);
        let read = window(&cells);
        assert_eq!(find(&read, 4, CAP_ID_LEGACY), Ok(Some(24)));
        let mut seen = [0u64; 2];
        let mut n = 0;
        for_each(&read, 4, CAP_ID_PROTOCOL, &mut |at| {
            seen[n] = at;
            n += 1;
        })
        .unwrap();
        assert_eq!((n, seen), (2, [16, 40]));
        assert_eq!(find(&read, 0, CAP_ID_LEGACY), Ok(None), "no list at all");
    }

    #[test]
    fn a_list_that_leaves_the_window_or_never_ends_is_refused() {
        assert_eq!(find(&window(&[0; 16]), 64, CAP_ID_LEGACY), Err(WalkError::OutOfWindow(256)));
        let mut jump = [0u32; 16];
        jump[4] = header(CAP_ID_PROTOCOL, 255);
        assert_eq!(find(&window(&jump), 4, CAP_ID_LEGACY), Err(WalkError::OutOfWindow(16 + 1020)));
        // All-ones is an unmapped read; it must refuse, not spin.
        assert_eq!(find(&window(&[u32::MAX; 16]), 4, CAP_ID_LEGACY), Err(WalkError::OutOfWindow(16 + 1020)));
        assert_eq!(find(&|_| Some(header(CAP_ID_PROTOCOL, 1)), 4, CAP_ID_LEGACY), Err(WalkError::TooMany));
    }

    /// The T14's two controllers read `0x01002201` before the OS asked: the OS
    /// semaphore already set, firmware's clear.
    #[test]
    fn the_handoff_is_named_by_firmwares_semaphore_before_and_after() {
        assert_eq!(handoff(0x0100_2201, 0x0100_2201), Handoff::NeverClaimed);
        assert_eq!(handoff(0x0001_2201, 0x0100_2201), Handoff::Released);
        assert_eq!(handoff(0x0001_2201, 0x0101_2201), Handoff::Kept);
    }

    /// The T14 latched all three status bits with every enable clear.
    #[test]
    fn every_smi_goes_off_and_every_latched_one_is_cleared() {
        assert_eq!(smis_off(0xe000_0000), SMI_STATUS);
        assert_eq!(smis_off(0xe000_e011) & SMI_ENABLES, 0);
        assert_eq!(smis_off(0x0000_e011), SMI_STATUS);
    }
}
