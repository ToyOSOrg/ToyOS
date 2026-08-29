//! Brings up every xHCI controller and runs the one enumeration scan that
//! happens before there is a scheduler; every wait here runs in place.

use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use crate::log;
use crate::time::{Budget, Cadence, Duration};
use crate::mm::paging::CachePolicy;
use crate::mm::Mmio;
use crate::drivers::pci::PciDevice;
use crate::drivers::DmaPool;
use super::super::{device, legacy};
use super::super::{storage_count, DEV_STRIDE, MSC_BLOCKS, PortState, Outstanding};
use device::{begin, reset_done, reset_port};
use super::super::{ErstEntry, Layout, MscBlock, PortMask, Portsc, Protocols, TrbRing};
use super::super::{XhciController, PAGE, RING_SIZE, USB_TIMEOUT_NS};
use super::super::{CAP_CAPLENGTH, CAP_DBOFF, CAP_HCCPARAMS1, CAP_HCSPARAMS1, CAP_HCSPARAMS2};
use super::super::{CAP_RTSOFF, HCC_PPC, XHCI_VECTOR};
use super::super::{IR0_ERDP, IR0_ERSTBA, IR0_ERSTSZ, IR0_IMAN, IR0_IMOD};
use super::super::{OFF_CMD_RING, OFF_DCBAA, OFF_ERST, OFF_EVT_RING};
use super::super::{OP_CONFIG, OP_CRCR, OP_DCBAAP, OP_PAGESIZE, OP_PORT_BASE, OP_USBCMD, OP_USBSTS};
use super::super::{PORTSC_PP, PORT_REG_SIZE, PORT_WORK_AT, XHCI};
use super::super::{controller_answers, PORT_DEBOUNCE_NS};
use super::settles;
use toyos_xhci::port::{self, GaveUp, Reset, ResetOutcome};
use toyos_xhci::Protocol;

/// How long a machine on which *nothing at all* has connected keeps looking.
///
/// Debounce alone can't tell an empty bus from a device still connecting: both
/// read as already-settled until something changes.
const EMPTY_BUS: Budget = Budget::of(
    Duration::from_secs(1),
    "the scan reports the bus as empty and the boot goes on without whatever was slow",
);

/// When the driver stops waiting for a root hub that keeps changing its mind.
pub const PORT_SETTLE_CEILING: Budget = Budget::of(
    Duration::from_millis(1_500),
    "the scan takes whatever is connected at that instant and names the port state",
);

/// How often the settle re-reads the port registers.
pub const PORT_POLL: Cadence = Cadence::every(
    Duration::from_millis(1),
    "one MMIO read per port, so sixteen reads per millisecond on the widest controller",
);
// `PORTSC.CCS` is not a question that can be asked at an instant: detecting a
// device is a physical process, so a scan issued right after `USBCMD.R/S`
// reports an empty bus on any machine whose ports are real.
//
// QEMU sets PORTSC from the QOM tree before the guest's first MMIO access, with
// no port state machine of its own — so no test in this suite would catch this
// wait being removed.
//
// Machine-wide, not per controller: the wait is wall-clock, so two controllers
// would otherwise pay for the same debounce twice.
fn await_connect_settle(controllers: &[XhciController]) {
    let Some(powered_at) = controllers.iter().map(|c| c.powered_at).max() else { return };
    let mut seen: Vec<(PortMask, u64)> = controllers
        .iter()
        .map(|c| (c.connected_ports(), c.powered_at))
        .collect();

    loop {
        let now = crate::clock::nanos_since_boot();
        let empty = seen.iter().all(|(mask, _)| *mask == [0u64; 4]);
        let debounced = seen
            .iter()
            .all(|(_, at)| now.saturating_sub(*at) >= PORT_DEBOUNCE_NS);
        let looked_long_enough = !empty || now.saturating_sub(powered_at) >= EMPTY_BUS.nanos();
        if debounced && looked_long_enough {
            return;
        }
        if now.saturating_sub(powered_at) >= PORT_SETTLE_CEILING.nanos() {
            log!("xHCI: no root hub on this machine held one connect state for {} ms within \
                 {} ms; enumerating whatever is connected now",
                PORT_DEBOUNCE_NS / 1_000_000, PORT_SETTLE_CEILING.duration().millis());
            return;
        }

        let next = now + PORT_POLL.nanos();
        while crate::clock::nanos_since_boot() < next {
            core::hint::spin_loop();
        }
        for (ctrl, (mask, changed_at)) in controllers.iter().zip(seen.iter_mut()) {
            let now_mask = ctrl.connected_ports();
            if now_mask != *mask {
                *mask = now_mask;
                *changed_at = crate::clock::nanos_since_boot();
            }
        }
    }
}

// `None` must stay a refusal, never a degradation: there is no polled mode, and
// every event-ring read depends on `irq_ring`, which only the ISR sets.
fn arm_interrupt(pci_dev: &PciDevice) -> Option<&'static str> {
    if pci_dev.enable_msix(XHCI_VECTOR) {
        return Some("MSI-X");
    }
    pci_dev.enable_msi(XHCI_VECTOR).then_some("MSI")
}

/// Brings up every xHCI controller on the machine, not just the first.
pub fn init(devices: &[PciDevice]) {
    // Once for the machine: it touches no device, so a second run would repeat itself.
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::xhci_descriptor_selftest() {
        device::selftest();
    }

    // All controllers power on before any is scanned: the settle wait is
    // wall-clock and shared, so scanning as each finishes would pay it twice.
    let mut controllers = Vec::new();
    let mut present = 0;
    for pci_dev in devices.iter().filter(|d| d.matches_class(0x0C, 0x03, Some(0x30))) {
        present += 1;
        if let Some(ctrl) = init_one(pci_dev) {
            controllers.push(ctrl);
        }
    }

    await_connect_settle(&controllers);

    for ctrl in controllers.iter_mut() {
        scan_ports(ctrl);
        if ctrl.devices.is_empty() {
            log!("xHCI: no HID devices on the controller at {:02x}:{:02x}.{}",
                ctrl.pci.bus, ctrl.pci.dev, ctrl.pci.func);
        }
    }

    if controllers.is_empty() {
        // Distinct log lines: a machine with no controller and one whose
        // controllers were all refused are different failures.
        match present {
            0 => log!("xHCI: no controller on this machine, USB input unavailable"),
            n => log!("xHCI: {n} controller(s) present, none of them usable, USB unavailable"),
        }
        return;
    }
    // Safe to zero: the boot scan acted on every port it looked at, so nothing is outstanding.
    PORT_WORK_AT.store(0, Ordering::Relaxed);
    let hid: usize = controllers.iter().map(|c| c.devices.len()).sum();
    log!("xHCI: {} controller(s), {} HID device(s)", controllers.len(), hid);
    log!("usb-storage: {} device(s)", storage_count());
    *XHCI.lock() = controllers;
}

// A controller that reports nothing leaves every port UNKNOWN, driven the USB2 way.
fn read_protocols(
    bar: &Mmio,
    bar_size: u64,
    hccparams1: u32,
    max_ports: u8,
    pci_dev: &PciDevice,
) -> Protocols {
    let read = |offset: u64| -> Option<u32> {
        (offset.checked_add(4)? <= bar_size).then(|| bar.read_u32(offset))
    };
    let mut protocols = Protocols::UNKNOWN;
    let mut refused = 0;
    let walked = legacy::for_each(
        &read,
        hccparams1 >> 16,
        legacy::CAP_ID_PROTOCOL,
        &mut |at| {
            let dwords = (read(at), read(at + 4), read(at + 8));
            let (Some(dw0), Some(dw1), Some(dw2)) = dwords else {
                refused += 1;
                return;
            };
            match toyos_xhci::protocol::SupportedProtocol::decode(dw0, dw1, dw2, max_ports) {
                Ok(found) => {
                    log!("xHCI: USB {}.{:x} on ports {}..={}", found.major, found.minor >> 4,
                        found.first_port + 1, found.first_port + found.port_count);
                    protocols.record(&found);
                }
                Err(why) => {
                    refused += 1;
                    log!("xHCI: a Supported Protocol capability at {at:#x} is unusable: {why:?}");
                }
            }
        },
    );
    if let Err(why) = walked {
        log!("xHCI: the capability list at PCI {:02x}:{:02x}.{} does not walk: {why:?}",
            pci_dev.bus, pci_dev.dev, pci_dev.func);
    }
    let (usb2, usb3) = protocols.counts(max_ports);
    log!("xHCI: {usb2} USB2 and {usb3} USB3 port register(s) of {max_ports} named, \
         {refused} capability(ies) refused");
    protocols
}

fn init_one(pci_dev: &PciDevice) -> Option<XhciController> {
    log!("xHCI: found at PCI {:02x}:{:02x}.{}", pci_dev.bus, pci_dev.dev, pci_dev.func);

    // xHCI 1.2 §5.2.1 puts the capability registers in BAR 0; a controller
    // that doesn't is one this driver cannot address.
    let bar_addr = match pci_dev.memory_bar(0) {
        Ok(memory) => memory.address(),
        Err(why) => {
            log!("xHCI: NOT INITIALISED at PCI {:02x}:{:02x}.{} — its capability registers are in \
                 BAR 0 and {}", pci_dev.bus, pci_dev.dev, pci_dev.func, why);
            return None;
        }
    };
    pci_dev.enable_bus_master();
    log!("xHCI: BAR0={:#x}", bar_addr);

    // Before reset and before the port scan: a controller whose interrupts
    // can't be delivered must not reach either.
    let Some(irq) = arm_interrupt(pci_dev) else {
        log!(
            "xHCI: NOT INITIALISED at PCI {:02x}:{:02x}.{} — the controller offers neither \
             MSI-X nor MSI, and this driver has no other way to be told it has anything to \
             say. No USB device on it can be used.",
            pci_dev.bus, pci_dev.dev, pci_dev.func
        );
        return None;
    };
    log!("xHCI: {irq} enabled (vector {XHCI_VECTOR:#x})");

    let bar = crate::mm::paging::map_mmio(bar_addr, 0x10000, CachePolicy::DeferToMtrr);

    let cap_length = bar.read_u8(CAP_CAPLENGTH) as u64;
    let hcsparams1 = bar.read_u32(CAP_HCSPARAMS1);
    let hcsparams2 = bar.read_u32(CAP_HCSPARAMS2);
    let hccparams1 = bar.read_u32(CAP_HCCPARAMS1);
    let db_offset = (bar.read_u32(CAP_DBOFF) & !0x3) as u64;
    let rts_offset = (bar.read_u32(CAP_RTSOFF) & !0x1F) as u64;

    let max_slots = (hcsparams1 & 0xFF) as u8;
    let max_ports = ((hcsparams1 >> 24) & 0xFF) as u8;
    let csz = ((hccparams1 >> 2) & 1) != 0;
    let context_size: usize = if csz { 64 } else { 32 };

    // Refuses this controller by name rather than taking the machine down with it.
    let refuse = |why: core::fmt::Arguments| {
        log!("xHCI: NOT INITIALISED at PCI {:02x}:{:02x}.{} — {why}. No USB device on it can \
             be used.", pci_dev.bus, pci_dev.dev, pci_dev.func);
    };

    // checked_sub, not `-`: an offset outside the 64 KiB window would otherwise
    // wrap to bar_size and pass Mmio::subregion's check, faulting on first use.
    let bar_size = 0x10000u64;
    let (Some(db_len), Some(rt_len)) =
        (bar_size.checked_sub(db_offset), bar_size.checked_sub(rts_offset))
    else {
        refuse(format_args!(
            "DBOFF={db_offset:#x} RTSOFF={rts_offset:#x} put its registers outside the \
             {bar_size:#x} window this driver maps"
        ));
        return None;
    };
    let op_base = bar.subregion(cap_length, bar_size - cap_length);
    let db_base = bar.subregion(db_offset, db_len);
    let rt_base = bar.subregion(rts_offset, rt_len);

    let pagesize = op_base.read_u32(OP_PAGESIZE) & 0xFFFF;
    log!("xHCI: max_slots={} max_ports={} ctx_size={} pagesize={:#x}",
        max_slots, max_ports, context_size, pagesize);
    // Tests that 4 KiB is included, not that it's the only supported size;
    // scratchpad buffer entries are one PAGE apart, so a controller without it
    // would corrupt memory silently.
    if pagesize & 1 == 0 {
        refuse(format_args!(
            "PAGESIZE={pagesize:#x} does not include 4 KiB, and every ring, context and \
             scratchpad buffer here is placed at 4 KiB"
        ));
        return None;
    }

    let max_sp_hi = ((hcsparams2 >> 21) & 0x1F) as usize;
    let max_sp_lo = ((hcsparams2 >> 27) & 0x1F) as usize;
    let layout = Layout::new((max_sp_hi << 5) | max_sp_lo, max_slots);
    log!("xHCI: dma {} KiB: scratchpad={} device blocks={} of {} B (max_slots={})",
        layout.pool_size / 1024, layout.scratch_count, layout.dev_blocks, DEV_STRIDE, max_slots);

    // Before any other access: firmware may still own the controller for
    // legacy keyboard emulation, and resetting under SMM is a fight with no diagnostic.
    legacy::take_ownership(&bar, bar_size, hccparams1);
    let protocols = read_protocols(&bar, bar_size, hccparams1, max_ports, pci_dev);

    let usbcmd = op_base.read_u32(OP_USBCMD);
    if usbcmd & 1 != 0 {
        op_base.write_u32(OP_USBCMD, usbcmd & !1);
    }
    let deadline_ms = USB_TIMEOUT_NS / 1_000_000;
    if !settles(|| controller_answers() && op_base.read_u32(OP_USBSTS) & 1 != 0) {
        refuse(format_args!("it never halted, within {deadline_ms} ms of being asked to"));
        return None;
    }

    op_base.write_u32(OP_USBCMD, 1 << 1);
    if !settles(|| controller_answers() && op_base.read_u32(OP_USBCMD) & (1 << 1) == 0) {
        refuse(format_args!("it held HCRST for {deadline_ms} ms"));
        return None;
    }
    if !settles(|| controller_answers() && op_base.read_u32(OP_USBSTS) & (1 << 11) == 0) {
        refuse(format_args!("it stayed Controller Not Ready for {deadline_ms} ms after its reset"));
        return None;
    }
    log!("xHCI: controller reset");

    // After the reset: a controller refused above never allocates, and
    // `DmaPool` frees the pool on every refusal below when it drops.
    let pool = DmaPool::alloc(layout.pool_size);

    // MaxSlotsEn caps what this driver can track; the controller then refuses
    // Enable Slot past it.
    op_base.write_u32(OP_CONFIG, layout.dev_blocks as u32);

    {
        let dma = pool.view();
        dma.zero();

        if layout.scratch_count > 0 {
            for i in 0..layout.scratch_count {
                let buf = dma.phys() + (layout.scratch_buffers + i * PAGE) as u64;
                // Must run before DCBAA[0] is written below: that's what tells
                // the controller the scratchpad array exists.
                dma.write::<u64>(layout.scratch_array + i * core::mem::size_of::<u64>(), buf);
            }
            // DCBAA slot 0 is the scratchpad array pointer, not a device context.
            super::super::write_dcbaa(dma, 0, dma.phys() + layout.scratch_array as u64);
            log!("xHCI: {} scratchpad buffers configured", layout.scratch_count);
        }

        op_base.write_u64(OP_DCBAAP, dma.phys() + OFF_DCBAA as u64);

        // CRCR bit 0 is RCS; the pointer is 64-byte aligned so `| 1` only sets
        // that bit (xHCI 1.2 §5.4.5).
        op_base.write_u64(OP_CRCR, (dma.phys() + OFF_CMD_RING as u64) | 1);

        // Must be written before IR0_ERSTBA below: that's what gives the
        // controller the table's address.
        dma.write::<ErstEntry>(OFF_ERST, ErstEntry {
            ring_base: dma.phys() + OFF_EVT_RING as u64,
            ring_size: RING_SIZE as u32,
            _reserved: 0,
        });
        rt_base.write_u32(IR0_ERSTSZ, 1);
        rt_base.write_u64(IR0_ERDP, dma.phys() + OFF_EVT_RING as u64);
        rt_base.write_u64(IR0_ERSTBA, dma.phys() + OFF_ERST as u64);

        rt_base.write_u32(IR0_IMOD, 0);
        rt_base.write_u32(IR0_IMAN, 3);

        op_base.write_u32(OP_USBCMD, 1 | (1 << 2));
    }
    if !settles(|| controller_answers() && op_base.read_u32(OP_USBSTS) & 1 == 0) {
        refuse(format_args!("it stayed halted for {deadline_ms} ms after R/S"));
        return None;
    }
    log!("xHCI: controller started");

    // No refusal after this point: `leak` gives up DmaPool's automatic free, so
    // a None return past here would leak the DMA pool.
    let dma = pool.leak();
    let cmd_ring = TrbRing::init(dma.subview(OFF_CMD_RING, PAGE));

    // HCRST leaves this controller's ports unpowered when it has Port Power
    // Control, and an unpowered port reports no device for the boot.
    //
    // PP is RW only with PPC; on a controller without it the write is a no-op
    // and the readback afterward is what says so.
    let mut powered = 0;
    for p in 0..max_ports {
        let off = OP_PORT_BASE + p as u64 * PORT_REG_SIZE;
        let portsc = op_base.read_u32(off);
        if portsc & PORTSC_PP == 0 {
            op_base.write_u32(off, Portsc::from_raw(portsc).neutral().powered().raw());
        }
        if op_base.read_u32(off) & PORTSC_PP != 0 {
            powered += 1;
        }
    }
    let powered_at = crate::clock::nanos_since_boot();
    log!("xHCI: {powered}/{max_ports} root-hub ports powered (PPC={})",
        u8::from(hccparams1 & HCC_PPC != 0));

    // Kept even with no HID bound: it is reset, started and armed, so dropping
    // it would leave a live interrupter with nothing draining its event ring.
    Some(XhciController {
        pci: *pci_dev,
        op_base,
        db_base,
        rt_base,
        max_ports,
        powered_at,
        context_size,
        layout,
        pool: dma,
        protocols,
        cmd_ring,
        event_ring: dma.subview(OFF_EVT_RING, PAGE),
        event_head: 0,
        event_phase: true,
        devices: Vec::new(),
        msc: [MscBlock::FREE; MSC_BLOCKS],
        ports: (0..max_ports)
            .map(|p| {
                let mut port = PortState::EMPTY;
                port.speaks(protocols.of(p));
                port
            })
            .collect(),
        ports_dirty: false,
        outstanding: Outstanding::EMPTY,
        software_disabled: [0u64; 4],
        held_event: None,
    })
}
/// Initialize and configure one USB device on a port, waiting for each step.
///
/// The reset kind is [`port::reset_needed`]'s answer alone, and what a
/// completion meant is [`port::reset_outcome`]'s: this path also runs during
/// boot, so a fix reaching only hot-plug would miss it.
pub fn init_device(ctrl: &mut XhciController, port_idx: u8, protocol: Option<Protocol>) {
    let Some(mut kind) = port::reset_needed(protocol, ctrl.read_portsc(port_idx)) else {
        log!("xHCI: port {} link already trained, no reset needed", port_idx + 1);
        return configure(ctrl, port_idx, None);
    };
    reset_port(ctrl, port_idx, kind);
    // At most two rounds: §4.19.5.1 has one escalation, hot to warm, and both
    // failure shapes below leave `kind` warm, from which neither retries.
    loop {
        // Bounded: a port that never finishes its reset costs that port, not the boot.
        if super::settles(|| reset_done(ctrl, port_idx)) {
            match port::reset_outcome(kind, protocol, ctrl.read_portsc(port_idx)) {
                ResetOutcome::Enumerate => return configure(ctrl, port_idx, Some(kind)),
                ResetOutcome::Escalate(write) => {
                    log!("xHCI: port {} failed its hot reset (PORTSC {:#010x}); warm resetting it",
                        port_idx + 1, ctrl.read_portsc(port_idx).raw());
                    ctrl.write_portsc(port_idx, write);
                    kind = Reset::Warm;
                    continue;
                }
                ResetOutcome::GaveUp(why) => {
                    match why {
                        GaveUp::LinkNeverTrained => log!(
                            "xHCI: port {} is SuperSpeed and its link would not train, warm \
                             reset included (PORTSC {:#010x}); skipping it",
                            port_idx + 1, ctrl.read_portsc(port_idx).raw()),
                        GaveUp::ResetFailed(k) => log!(
                            "xHCI: port {} completed its {} reset without enabling \
                             (PORTSC {:#010x}); skipping it",
                            port_idx + 1,
                            match k { Reset::Hot => "hot", Reset::Warm => "warm" },
                            ctrl.read_portsc(port_idx).raw()),
                        GaveUp::ResetNeverFinished(_) => {
                            unreachable!("a completed reset cannot have never finished")
                        }
                    }
                    return ctrl.port_bound(port_idx, None);
                }
            }
        }
        // xHCI 1.2 §4.19.1.2.4: a SuperSpeed link left Inactive by a failed hot
        // reset needs a warm reset; without this the port is lost for the boot.
        if kind == Reset::Hot && protocol == Some(Protocol::Usb3) {
            log!("xHCI: port {} did not take a hot reset (link {:?}); warm resetting it",
                port_idx + 1, ctrl.read_portsc(port_idx).link_state());
            reset_port(ctrl, port_idx, Reset::Warm);
            kind = Reset::Warm;
            continue;
        }
        log!("xHCI: port {} never finished its reset (PORTSC {:#010x}); skipping it",
            port_idx + 1, ctrl.read_portsc(port_idx).raw());
        return ctrl.port_bound(port_idx, None);
    }
}

/// Everything between a port that has just finished its reset and a device the
/// driver can use, run to the end in place.
pub fn configure(ctrl: &mut XhciController, port_idx: u8, after: Option<Reset>) {
    begin(ctrl, port_idx, after);
    ctrl.settle_outstanding();
}
/// Scans every port on the controller and initializes each connected device.
///
/// Serial by construction: the input context, EP0 ring and descriptor buffer
/// are reused across ports.
pub fn scan_ports(ctrl: &mut XhciController) {
    for p in 0..ctrl.max_ports {
        // No speed printed here: xHCI 1.2 §4.19.5 says Port Speed isn't valid
        // until PR transitions 1→0, so QEMU's early value would read as fact
        // and isn't.
        if ctrl.read_portsc(p).connected() {
            log!("xHCI: port {} connected", p + 1);
            init_device(ctrl, p, ctrl.protocols.of(p));
        }
    }
    // Must run: a change flag raises an event only on 0→1, so a CSC left set
    // from boot-time attach would make the first unplug after boot go unreported.
    ctrl.acknowledge_port_changes();
    // Completions from an earlier port's device can arrive during a later
    // port's enumeration; without this drain a broken one goes unrecorded.
    ctrl.settle_outstanding();
    // After acknowledge_port_changes: the connect this raises must be a change
    // the port machine sees, not one the scan just cleared.
    if crate::actuator::xhci_slow_storage_connect() {
        super::super::BOOT_SCAN_DONE.store(true, core::sync::atomic::Ordering::Relaxed);
    }
    if crate::actuator::xhci_portsc_rw1c() {
        log!("xHCI: PED as RW1C, {} port(s) disabled by a driver write",
            ctrl.software_disabled_ports());
    }
}

