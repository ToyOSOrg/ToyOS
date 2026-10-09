
use toyos_usbhid::{keyboard::Keyboard, pointer::{Motion, Pointer}};

use crate::{keyboard, log, mouse};
use super::{Mmio, Trb, TrbRing, TRB_NORMAL};

/// What a configuration descriptor's HID interface said it was, differing only in report size and SET_PROTOCOL; parse-time only.
#[derive(Clone, Copy, PartialEq)]
pub enum HidType {
    Keyboard,
    Mouse,
    Tablet,
}

/// What a *bound* device is, with what its reports are read against: a keyboard's own last report, since diffing against another device's would synthesize releases for keys still down; and a pointer's source, carried rather than derived from the (per-controller) slot id.
#[derive(Clone, Copy)]
pub enum HidRole {
    Keyboard(Keyboard),
    Pointer(mouse::PointerSource, Pointer),
}

pub struct HidDevice {
    pub slot_id: u8,
    /// The root-hub port this device is on; unlike the slot id, survives a disable.
    pub port_idx: u8,
    /// PORTSC's Port Speed when the port came up, decoded once at bind: a
    /// psiv with no default Protocol Speed ID refused the port before a slot
    /// was ever spent on it.
    pub speed: toyos_abi::inventory::UsbSpeed,
    /// What its device descriptor says it is.
    pub usb: toyos_xhci::identity::UsbId,
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
    /// Reports refused since bind; only the first and each power of two after it is logged, so a device cannot flood the log.
    pub refused: u32,
    /// The completion code this endpoint broke with; read and cleared by [`super::XhciController::recover_endpoints`], never by the code that sets it.
    pub broke_with: Option<u32>,
    /// Consecutive failures; a delivered report clears it — see [`super::MAX_HID_FAILURES`].
    pub failures: u8,
}

impl HidDevice {
    /// What this device is called in every line about it: two names, not three — mouse and tablet dispatch identically, distinguished only by report length.
    pub fn kind(&self) -> &'static str {
        match self.role {
            HidRole::Keyboard(_) => "keyboard",
            HidRole::Pointer(..) => "pointer",
        }
    }

    /// Reads the report a transfer delivered; `unmoved` is what of `report_size` the device did not send, which still holds the last report's bytes.
    pub fn dispatch_report(&mut self, unmoved: u32) {
        let mut buf = [0u8; 8];
        // `report_size` is 4, 6 or 8, so `copy_to` never sees more than 8; not yet requeued, so this copy has the buffer to itself.
        let delivered = &mut buf[..self.report_size.saturating_sub(unmoved) as usize];
        self.report.copy_to(0, delivered);
        // Waking on an unchanged report would make readiness disagree with `has_data()`.
        let queued = match &mut self.role {
            HidRole::Keyboard(keys) => match keys.report(delivered) {
                Ok(transitions) => keyboard::apply(transitions) != 0,
                Err(why) => return self.refuse(format_args!("{why:?}")),
            },
            HidRole::Pointer(source, pointer) => match pointer.decode(delivered) {
                Ok(read) => mouse::handle_motion(*source, read.buttons, motion(read.motion), read.wheel),
                Err(why) => return self.refuse(format_args!("{why:?}")),
            },
        };
        if queued {
            self.wake();
        }
    }

    #[cold]
    fn refuse(&mut self, why: core::fmt::Arguments<'_>) {
        self.refused = self.refused.saturating_add(1);
        if self.refused.is_power_of_two() {
            log!("xHCI: slot {} {} report refused, {} since it bound: {why}",
                self.slot_id, self.kind(), self.refused);
        }
    }

    /// Releases everything this device was holding, on its way off the bus.
    /// Its own keys, not `keyboard::release_all`: only its `Keyboard` records which held keys are this device's.
    pub fn unbind(&mut self) {
        let queued = match &mut self.role {
            HidRole::Keyboard(keys) => keyboard::apply(keys.release()) != 0,
            // Also frees this device's button-table entry, so replugging costs the machine nothing.
            HidRole::Pointer(source, _) => mouse::unbind(*source),
        };
        if queued {
            self.wake();
        }
    }

    // The role's one watch, which carries the blocked `sys_read` and the polls alike.
    fn wake(&self) {
        match self.role {
            HidRole::Keyboard(_) => crate::keyboard::WATCH.post(),
            HidRole::Pointer(..) => crate::mouse::WATCH.post(),
        }
    }

    pub fn requeue(&mut self, db_base: &Mmio) {
        let mut trb = Trb::ZERO;
        trb.param = self.report.device_addr();
        trb.status = self.report_size;
        trb.control = TRB_NORMAL | (1 << 5); // IOC
        self.int_ring.enqueue(trb);
        // An `Mmio` write: ordered after the TRB it announces.
        db_base.write_u32(self.slot_id as u64 * 4, self.int_ep_dci as u32);
    }
}

fn motion(motion: Motion) -> mouse::Motion {
    match motion {
        Motion::Relative { dx, dy } => mouse::Motion::Relative { dx: dx.into(), dy: dy.into() },
        Motion::Absolute { x, y } => mouse::Motion::Absolute { x, y },
    }
}
