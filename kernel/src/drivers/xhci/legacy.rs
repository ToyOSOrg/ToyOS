//! Takes the xHC from firmware before reset (xHCI spec §4.22.1).
//!
//! The extended capability list is firmware's, so it is untrusted: nothing
//! here panics or loops unbounded on firmware-supplied data, and a malformed
//! list costs the handoff, never the boot.

use crate::log;
use crate::mm::Mmio;
use crate::time::{Budget, Duration};

/// Capability ID 1 — USB Legacy Support (spec §7.1.1).
const CAP_ID_LEGACY: u8 = 1;

/// USBLEGSUP bit 16: firmware owns the controller.
const LEGSUP_BIOS_OWNED: u32 = 1 << 16;
/// USBLEGSUP bit 24: the OS claims it.
const LEGSUP_OS_OWNED: u32 = 1 << 24;

/// USBLEGCTLSTS sits one dword past USBLEGSUP (spec §7.1.2).
const LEGCTLSTS: u64 = 4;
/// Its SMI enables: USB SMI, host-system-error, OS-ownership, PCI-command, BAR.
const SMI_ENABLES: u32 = (1 << 0) | (1 << 4) | (1 << 13) | (1 << 14) | (1 << 15);
/// Its write-1-to-clear SMI status bits: OS ownership change, PCI command, BAR.
const SMI_STATUS: u32 = (1 << 29) | (1 << 30) | (1 << 31);

/// Grace period for firmware to release the controller; expiry is not fatal.
const HANDOFF_TIMEOUT: Budget = Budget::of(
    Duration::from_secs(1),
    "the controller is reset out from under firmware, with a line naming the semaphore",
);

/// Iteration cap for the capability walk, independent of firmware's link count.
const MAX_CAPS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkError {
    /// A capability header outside the mapped register window.
    OutOfWindow(u64),
    /// More links than `MAX_CAPS`.
    TooMany,
}

/// Byte offset of the first capability with `id`, or `None` if the list has none.
/// `read` must return `None` for any offset outside the mapped window.
pub fn find(
    read: &dyn Fn(u64) -> Option<u32>,
    xecp_dwords: u32,
    id: u8,
) -> Result<Option<u64>, WalkError> {
    let mut found = None;
    walk(read, xecp_dwords, id, &mut |at| {
        found = Some(at);
        false
    })?;
    Ok(found)
}

/// Every capability with `id`, in list order; an id may appear more than once.
pub fn for_each(
    read: &dyn Fn(u64) -> Option<u32>,
    xecp_dwords: u32,
    id: u8,
    visit: &mut dyn FnMut(u64),
) -> Result<(), WalkError> {
    walk(read, xecp_dwords, id, &mut |at| {
        visit(at);
        true
    })
}

/// `visit` returns `false` to stop the walk early.
fn walk(
    read: &dyn Fn(u64) -> Option<u32>,
    xecp_dwords: u32,
    id: u8,
    visit: &mut dyn FnMut(u64) -> bool,
) -> Result<(), WalkError> {
    if xecp_dwords == 0 {
        return Ok(());
    }
    let mut offset = xecp_dwords as u64 * 4;
    for _ in 0..MAX_CAPS {
        let Some(header) = read(offset) else {
            return Err(WalkError::OutOfWindow(offset));
        };
        if header as u8 == id && !visit(offset) {
            return Ok(());
        }
        let next = (header >> 8) & 0xFF;
        if next == 0 {
            return Ok(());
        }
        // Offset strictly increases (1..=255 dwords per step) and cannot wrap.
        offset += next as u64 * 4;
    }
    Err(WalkError::TooMany)
}

/// Capability ID 2 — Supported Protocol (spec §7.2).
pub const CAP_ID_PROTOCOL: u8 = 2;

/// Asks firmware for the controller and disables its SMIs regardless of the answer.
/// Never refuses: the caller resets the controller whether or not the handoff succeeded.
pub fn take_ownership(bar: &Mmio, bar_size: u64, hccparams1: u32) {
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::xhci_xecp_selftest() {
        selftest();
    }

    let xecp = hccparams1 >> 16;
    let read = |offset: u64| -> Option<u32> {
        (offset.checked_add(4)? <= bar_size).then(|| bar.read_u32(offset))
    };
    let legsup = match find(&read, xecp, CAP_ID_LEGACY) {
        Ok(Some(offset)) => offset,
        Ok(None) => {
            log!("xHCI: no USB Legacy Support capability (xECP={xecp:#x}), nothing to hand over");
            return;
        }
        Err(e) => {
            log!("xHCI: extended capability list unusable ({e:?}, xECP={xecp:#x}) — no handoff");
            return;
        }
    };
    // USBLEGCTLSTS is the dword after the one `find` bounds-checked.
    if legsup + LEGCTLSTS + 4 > bar_size {
        log!("xHCI: USB Legacy Support at {legsup:#x} runs past the register window — no handoff");
        return;
    }

    let before = bar.read_u32(legsup);
    bar.write_u32(legsup, before | LEGSUP_OS_OWNED);

    let started = crate::clock::nanos_since_boot();
    let deadline = started + HANDOFF_TIMEOUT.nanos();
    let mut now = bar.read_u32(legsup);
    while now & LEGSUP_BIOS_OWNED != 0 && crate::clock::nanos_since_boot() < deadline {
        core::hint::spin_loop();
        now = bar.read_u32(legsup);
    }
    let waited_us = (crate::clock::nanos_since_boot() - started) / 1_000;

    if now & LEGSUP_BIOS_OWNED != 0 {
        // Not fatal: the controller resets regardless of what firmware reports.
        log!(
            "xHCI: firmware still owns the controller after {}ms (USBLEGSUP {before:#010x} -> {now:#010x}) — resetting it anyway",
            HANDOFF_TIMEOUT.duration().millis()
        );
    } else if before & LEGSUP_BIOS_OWNED != 0 {
        log!("xHCI: firmware released the controller in {waited_us}us (USBLEGSUP {before:#010x} -> {now:#010x})");
    } else {
        log!("xHCI: firmware did not claim the controller (USBLEGSUP {before:#010x})");
    }

    // SMIs are disabled whatever the ownership result; the two bits are independent.
    let ctl = bar.read_u32(legsup + LEGCTLSTS);
    bar.write_u32(legsup + LEGCTLSTS, (ctl & !SMI_ENABLES) | SMI_STATUS);
    let after = bar.read_u32(legsup + LEGCTLSTS);
    log!("xHCI: USBLEGCTLSTS {ctl:#010x} -> {after:#010x} (SMI generation off)");
}

/// Malformed capability lists QEMU's xHC cannot produce; exercises [`find`]'s bounds checks.
#[cfg(feature = "boot-actuators")]
fn selftest() {
    /// A 16-dword register window, so a dword at or past 0x40 is outside it.
    const WINDOW: u64 = 64;
    const CASES: usize = 8;

    fn header(id: u8, next: u8) -> u32 {
        id as u32 | ((next as u32) << 8)
    }

    let mut passed = 0usize;
    let mut check = |name: &str,
                     read: &dyn Fn(u64) -> Option<u32>,
                     xecp: u32,
                     want: Result<Option<u64>, WalkError>| {
        let got = find(read, xecp, CAP_ID_LEGACY);
        if got == want {
            passed += 1;
        } else {
            log!("xHCI: xecp selftest FAILED on {name}: got {got:?}, want {want:?}");
        }
    };

    // No case starts the list at dword 0 — real controllers occupy the first few.
    fn windowed(cells: &[u32; 16]) -> impl Fn(u64) -> Option<u32> + '_ {
        move |offset: u64| -> Option<u32> {
            if !offset.is_multiple_of(4) || offset.checked_add(4)? > WINDOW {
                return None;
            }
            Some(cells[(offset / 4) as usize])
        }
    }

    check("no list at all", &windowed(&[0; 16]), 0, Ok(None));

    let mut one = [0u32; 16];
    one[4] = header(2, 0);
    check("one capability, not ours", &windowed(&one), 4, Ok(None));

    let mut third = [0u32; 16];
    third[4] = header(2, 2);
    third[6] = header(2, 4);
    third[10] = header(CAP_ID_LEGACY, 0);
    check("ours, third in the list", &windowed(&third), 4, Ok(Some(40)));

    let mut last = [0u32; 16];
    last[4] = header(2, 11);
    last[15] = header(CAP_ID_LEGACY, 0);
    check("ours in the last dword there is", &windowed(&last), 4, Ok(Some(60)));

    check(
        "xECP points outside the window",
        &windowed(&[0; 16]),
        64,
        Err(WalkError::OutOfWindow(256)),
    );

    let mut jump = [0u32; 16];
    jump[4] = header(2, 255);
    check(
        "a link that leaves the window",
        &windowed(&jump),
        4,
        Err(WalkError::OutOfWindow(16 + 1020)),
    );

    // All-ones is what an unmapped region reads as; it must refuse, not spin.
    check(
        "a window reading all ones",
        &windowed(&[u32::MAX; 16]),
        4,
        Err(WalkError::OutOfWindow(16 + 1020)),
    );

    // Conformant but endless; only the iteration cap can end it, the window check never fires.
    check(
        "an endless chain inside a window that never ends",
        &|_| Some(header(2, 1)),
        4,
        Err(WalkError::TooMany),
    );

    log!("xHCI: xecp selftest {passed}/{CASES} malformed lists refused");
}
