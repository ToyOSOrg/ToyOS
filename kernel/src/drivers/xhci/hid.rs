use core::sync::atomic::{fence, Ordering};

use crate::{keyboard, mouse};
use super::{Mmio, Trb, TrbRing, TRB_NORMAL};

/// What a configuration descriptor's HID interface said it was, differing only in report size and SET_PROTOCOL; parse-time only.
#[derive(Clone, Copy, PartialEq)]
pub enum HidType {
    Keyboard,
    Mouse,
    Tablet,
}

/// What a *bound* device is; pointer and tablet dispatch identically, so the source is carried rather than derived from the (per-controller) slot id.
#[derive(Clone, Copy)]
pub enum HidRole {
    Keyboard,
    Pointer(mouse::PointerSource),
}

pub struct HidDevice {
    pub slot_id: u8,
    /// The root-hub port this device is on; unlike the slot id, survives a disable.
    pub port_idx: u8,
    /// This device's block in the DMA pool: interrupt ring, EP0 ring and output context.
    pub block: usize,
    pub int_ep_dci: u8,
    /// The device's own endpoint address, distinct from the controller's DCI; what CLEAR_FEATURE(ENDPOINT_HALT) needs.
    pub ep_addr: u8,
    pub int_ring: TrbRing,
    /// The device's control ring, kept past enumeration: clearing a halt is a control transfer.
    pub ep0_ring: TrbRing,
    /// The eight-byte DMA slot the interrupt endpoint delivers reports into; a Dma view bounds accesses against its own length, not `report_size`.
    pub report: crate::mm::Dma<'static>,
    pub report_size: u32,
    pub role: HidRole,
    /// Per device: diffing against another device's report would synthesize releases for keys still down.
    pub prev_report: [u8; 8],
    /// The completion code this endpoint broke with; read and cleared by [`super::XhciController::recover_endpoints`], never by the code that sets it.
    pub broke_with: Option<u32>,
    /// Consecutive failures; a delivered report clears it — see [`super::MAX_HID_FAILURES`].
    pub failures: u8,
    /// Completions this endpoint has produced.
    /// Counted unconditionally so the `xhci-hid-break-*` actuators aren't a second code path.
    #[cfg_attr(not(feature = "boot-actuators"), allow(dead_code))]
    pub completions: u32,
}

impl HidDevice {
    /// What this device is called in every line about it: two names, not three — mouse and tablet dispatch identically, distinguished only by report length.
    pub fn kind(&self) -> &'static str {
        match self.role {
            HidRole::Keyboard => "keyboard",
            HidRole::Pointer(_) => "pointer",
        }
    }

    pub fn dispatch_report(&mut self) {
        let mut buf = [0u8; 8];
        let size = self.report_size as usize;
        // `report_size` is 4, 6 or 8, so `copy_to` never sees `size > 8`; not yet requeued, so this copy has the buffer to itself.
        self.report.copy_to(0, &mut buf[..size]);
        // Waking on an unchanged report would make readiness disagree with `has_data()`.
        let queued = match self.role {
            HidRole::Keyboard => keyboard::handle_report(&mut self.prev_report, &buf[..size]) != 0,
            HidRole::Pointer(source) => mouse::handle_report(source, &buf[..size]) != 0,
        };
        if queued {
            self.wake();
        }
    }

    /// Releases everything this device was holding, on its way off the bus.
    /// A zero report, not `keyboard::release_all`: only `prev_report` records which held keys are this device's.
    pub fn unbind(&mut self) {
        let queued = match self.role {
            HidRole::Keyboard => keyboard::handle_report(&mut self.prev_report, &[0u8; 8]) != 0,
            // Also frees this device's button-table entry, so replugging costs the machine nothing.
            HidRole::Pointer(source) => mouse::unbind(source),
        };
        if queued {
            self.wake();
        }
    }

    // A keyboard wakes both unpaired halves — the blocked-`sys_read` queue and the poll watchers;
    // a pointer has only the poll half: an empty Mouse read answers `NotFound`, never parks.
    fn wake(&self) {
        let (watchers, source) = match self.role {
            HidRole::Keyboard => {
                keyboard::wake_waiters();
                (keyboard::inbox_watchers(), crate::inbox::Source::Keyboard)
            }
            HidRole::Pointer(_) => (mouse::inbox_watchers(), crate::inbox::Source::Mouse),
        };
        if !watchers.is_empty() {
            crate::inbox::complete_pending_for_event(&watchers, source);
        }
    }

    pub fn requeue(&mut self, db_base: &Mmio) {
        let mut trb = Trb::ZERO;
        trb.param = self.report.phys();
        trb.status = self.report_size;
        trb.control = TRB_NORMAL | (1 << 5); // IOC
        self.int_ring.enqueue(trb);
        fence(Ordering::Release);
        db_base.write_u32(self.slot_id as u64 * 4, self.int_ep_dci as u32);
    }
}

/// Takes one completion away from the device that earned it and hands the driver a stall in its place.
// QEMU's usb-hid has no path to USB_RET_STALL for an interrupt IN token, so nothing on the host side can stage this.
// Replaces the completion code and the delivered report, not the TRB/ring/transfer-event/output-context chain, so a dispatched "success" can only be real.
#[cfg(feature = "boot-actuators")]
impl HidDevice {
    // The first completion is a never-delivered endpoint; the fourth is one that was working and stopped — different driver states, not degrees of one.
    fn break_at() -> Option<u32> {
        if crate::actuator::xhci_hid_break_first() {
            Some(1)
        } else if crate::actuator::xhci_hid_break_late() {
            Some(4)
        } else {
            None
        }
    }

    pub fn stage_break(&mut self, code: u32) -> u32 {
        self.completions += 1;
        if Self::break_at() != Some(self.completions) {
            return code;
        }
        // Zeroing leaves the slot as a stalled endpoint would have; runs before requeue, so nothing else touches the buffer.
        self.report.subview(0, self.report_size as usize).zero();
        super::CC_STALL
    }
}
