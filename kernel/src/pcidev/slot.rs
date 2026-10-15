//! The hot-plug slots this kernel drives: a PCI Express port's slot (PCIe base
//! spec 6.0 §6.7), armed only below a host bridge whose `_OSC` granted the
//! kernel native hot-plug and PCI Express capability structure control (ACPI
//! 6.5 §6.2.11; PCI Firmware Specification 3.3 §4.5.1).
//!
//! **The port's own interrupt, on the claim path's remapped message, and a
//! bounded handler** ([`HotPlug::isr`]): it reads and clears Slot Status,
//! takes the functions behind a slot that lost its card or whose button asked
//! for removal out of the device set — every call their claims make answers
//! `Gone` from there — issues at most one Slot Control write a command was
//! waiting on, and posts the set's watch. Nothing else runs there.
//!
//! **Everything that may take longer runs on the reader of the device set**
//! ([`refresh`]), charged to it: powering a slot, §6.6.1's wait after its link
//! comes up, which is a time in the snapshot the reader waits to and never a
//! spin, and reading the functions below it. What a slot is owed is
//! `toyos_pci::hotplug::next`'s.
//!
//! **Removal is the button's or the card's**: a press on a powered slot whose
//! functions are listed removes them at once, and the slot is powered off when
//! the last claim on them is released. §6.7.1.5's five seconds in which a
//! second press cancels the removal are not offered.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use toyos_abi::inventory::Arrival;
use toyos_pci::bridge::Bridge;
use toyos_pci::hotplug::{self, Next, Seen};

use super::{function, functions, record, requester, Armed, Function, Refusal, MACHINE};
use crate::drivers::pci::{arm_claimed_msi, arm_claimed_msix, NoCapability, NoEntry, PciDevice, MSIX_ENTRY};
use crate::sync::{Lock, Masked};

/// PCI Firmware Specification 3.3 §4.5.1, the `_OSC` Control field of a PCI
/// host bridge: native hot-plug control, and control of the PCI Express
/// capability structure, in which Slot Control lives.
pub const NATIVE_HOT_PLUG: u32 = 1 << 0;
pub const CAPABILITY_STRUCTURE: u32 = 1 << 4;

/// A PCI host bridge as its declaration describes it.
pub struct HostBridge {
    pub segment: u16,
    pub buses: core::ops::RangeInclusive<u8>,
    /// What its `_OSC` granted: `None` where it has none.
    pub granted: Option<u32>,
}

/// Every slot armed, for the reader to walk.
static SLOTS: Lock<Vec<&'static HotPlug>> = Lock::new(Vec::new());

/// One armed slot: its port's registers, and what its handler and the device
/// set's reader share.
pub(super) struct HotPlug {
    pci: PciDevice,
    express: u64,
    slot: hotplug::Slot,
    secondary: u8,
    subordinate: u8,
    /// Kept for the boot: the port's message, and its MSI-X entry, the
    /// kernel's alone.
    _armed: Armed,
    removing: AtomicBool,
    /// When the reader first saw the link below up in a powered, occupied
    /// slot; 0 while it is not.
    link_since: AtomicU64,
    command: Masked<Command>,
}

/// A Slot Control write waiting on the last one's Command Completed
/// (§6.7.3.2), and when the last one was made.
struct Command {
    wanted: Option<u16>,
    since: Option<u64>,
}

/// Arm the slot of every port below a host bridge whose `_OSC` granted both
/// controls, and say for every port why its slot is or is not armed.
pub fn arm(bridges: &[HostBridge]) {
    let ports: Vec<(usize, u64, hotplug::Slot, Bridge, u16)> = {
        let machine = MACHINE.lock();
        machine.ports.iter().filter(|port| !port.armed).map(|p| (p.number, p.express, p.slot, p.bridge, machine.segment)).collect()
    };
    log!("pcidev: {} host bridge(s) declared, {} hot-plug slot(s) not yet armed", bridges.len(), ports.len());
    for bridge in bridges {
        match bridge.granted {
            Some(granted) => log!(
                "pcidev: host bridge on segment {:#06x} buses {:#04x}..={:#04x}: its _OSC granted {granted:#x}",
                bridge.segment,
                bridge.buses.start(),
                bridge.buses.end()
            ),
            None => log!(
                "pcidev: host bridge on segment {:#06x} buses {:#04x}..={:#04x}: no _OSC, so no native hot-plug below it",
                bridge.segment,
                bridge.buses.start(),
                bridge.buses.end()
            ),
        }
    }
    for (number, express, slot, bridge, segment) in ports {
        let port = function(number);
        let declared = bridges.iter().find(|b| b.segment == segment && b.buses.contains(&bridge.bus));
        let granted = match declared.map(|b| b.granted) {
            None => {
                log!("pcidev: {port}'s hot-plug slot is not armed: no host bridge declared covers bus {:#04x}", bridge.bus);
                continue;
            }
            Some(None) => {
                log!("pcidev: {port}'s hot-plug slot is not armed: its host bridge has no _OSC to grant native hot-plug");
                continue;
            }
            Some(Some(granted)) => granted,
        };
        let needed = NATIVE_HOT_PLUG | CAPABILITY_STRUCTURE;
        if granted & needed != needed {
            log!(
                "pcidev: {port}'s hot-plug slot is not armed: its host bridge's _OSC granted {granted:#x}, \
                 without native hot-plug and capability structure control ({needed:#x})"
            );
            continue;
        }
        match arm_one(number, port, express, slot, bridge) {
            Ok(armed) => {
                MACHINE.lock().ports.iter_mut().filter(|p| p.number == number).for_each(|p| p.armed = true);
                SLOTS.lock().push(armed);
                log!("pcidev: {port}'s hot-plug slot is armed: its host bridge's _OSC granted {granted:#x}");
                armed.command(Some(slot.control(armed.seen().occupied && armed.powered())), false);
                super::changed();
            }
            Err(why) => log!("pcidev: {port}'s hot-plug slot is not armed: {why}"),
        }
    }
}

/// Take `port` for this kernel and arm its slot's interrupt, on the message
/// a claim's would be: remapped for the port alone.
fn arm_one(number: usize, port: &'static Function, express: u64, slot: hotplug::Slot, bridge: Bridge) -> Result<&'static HotPlug, String> {
    if usize::from(slot.message) != usize::from(MSIX_ENTRY) {
        return Err("its slot signals on a message other than the one entry this kernel arms".into());
    }
    if port.held.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        return Err("a claim holds the port".into());
    }
    let pci = port.pci;
    let refused = |why: Refusal| {
        port.held.store(false, Ordering::Release);
        why.to_string()
    };
    let remapping = crate::iommu::remapping().map_err(|why| refused(Refusal::NotRemapped(why)))?;
    crate::iommu::isolates(&pci).map_err(|why| refused(Refusal::NotIsolated(why)))?;
    let message = crate::iommu::claim_msi(remapping, number, &pci).map_err(|why| refused(Refusal::NoMessage(why)))?;
    pci.enable_memory_space();
    let armed = match arm_claimed_msix(&message) {
        Ok(at) => Armed::Msix(at, message),
        Err(NoEntry::NoTable(NoCapability::Absent)) if arm_claimed_msi(&message) => Armed::Msi(message),
        Err(_) => return Err(refused(Refusal::NoInterrupt)),
    };
    // A port's own message is a write it masters (PCI 3.0 §6.2.2, bit 2),
    // and what arrives behind it reaches memory through it.
    pci.enable_bus_master();
    let armed: &'static HotPlug = alloc::boxed::Box::leak(alloc::boxed::Box::new(HotPlug {
        pci,
        express,
        slot,
        secondary: bridge.secondary,
        subordinate: bridge.subordinate,
        _armed: armed,
        removing: AtomicBool::new(false),
        link_since: AtomicU64::new(0),
        command: Masked::new(Command { wanted: None, since: None }),
    }));
    armed.write_status(hotplug::EVENTS);
    port.slot.store(core::ptr::from_ref(armed).cast_mut(), Ordering::Release);
    Ok(armed)
}

/// The slot whose port is `function`, if it is one.
pub(super) fn of(function: &Function) -> Option<&'static HotPlug> {
    let slot = function.slot.load(Ordering::Acquire);
    // SAFETY: non-null only as `arm_one` stored it: a leaked box, never freed.
    (!slot.is_null()).then(|| unsafe { &*slot })
}

/// The armed slot `function` is behind, if any.
pub(super) fn above(function: &Function) -> Option<&'static HotPlug> {
    SLOTS.lock().iter().copied().find(|slot| slot.behind(function))
}

/// Act on what every armed slot is owed, and answer how long until one may
/// change with no interrupt to say so.
pub(super) fn refresh() -> Option<u64> {
    let slots: Vec<&'static HotPlug> = SLOTS.lock().clone();
    slots.iter().filter_map(|slot| slot.refresh()).min()
}

impl HotPlug {
    fn at(&self, register: u64) -> u64 {
        self.express + register
    }

    fn write_status(&self, events: u16) {
        self.pci.write_config_u16(self.at(hotplug::SLOT_STATUS), events);
    }

    fn behind(&self, function: &Function) -> bool {
        (self.secondary..=self.subordinate).contains(&function.pci.bus)
    }

    fn powered(&self) -> bool {
        self.slot.powered(self.pci.read_config_u16(self.at(hotplug::SLOT_CONTROL)))
    }

    /// The slot as its registers and the device set say it is.
    fn seen(&self) -> Seen {
        let status = self.pci.read_config_u16(self.at(hotplug::SLOT_STATUS));
        let behind = || functions().filter(|function| self.behind(function));
        Seen {
            occupied: hotplug::occupied(status),
            powered: self.powered(),
            link_up: self.slot.link_up(self.pci.read_config_u16(self.at(hotplug::LINK_STATUS))),
            listed: behind().any(|function| function.arrival.load(Ordering::Acquire) != 0),
            held: behind().any(|function| function.held.load(Ordering::Acquire)),
            removing: self.removing.load(Ordering::Acquire),
        }
    }

    /// Take every function behind the slot out of the device set, and wake
    /// whoever waits on one so it reads `Gone`.
    fn depart(&self) {
        for function in functions().filter(|function| self.behind(function)) {
            let arrival = function.arrival.swap(0, Ordering::AcqRel);
            if arrival != 0 {
                function.left.store(arrival, Ordering::Release);
                function.watch.post_in_place();
            }
        }
    }

    /// The port's interrupt: bounded, allocating nothing and taking no lock
    /// but the command's, which masks interrupts.
    pub(super) fn isr(&self) {
        let status = self.pci.read_config_u16(self.at(hotplug::SLOT_STATUS));
        let events = status & hotplug::EVENTS;
        if events == 0 {
            return;
        }
        self.write_status(events);
        if events & hotplug::PRESENCE_CHANGED != 0 && !hotplug::occupied(status) {
            self.removing.store(false, Ordering::Release);
            self.depart();
        }
        if events & hotplug::BUTTON_PRESSED != 0 {
            let listed = functions().any(|function| self.behind(function) && function.arrival.load(Ordering::Acquire) != 0);
            let seen = Seen { listed, powered: self.powered(), ..Seen::default() };
            let removes = hotplug::pressed_removes(seen);
            self.removing.store(removes, Ordering::Release);
            if removes {
                self.depart();
            }
        }
        self.command(None, events & hotplug::COMMAND_COMPLETED != 0);
        super::changed();
    }

    /// Ask for Slot Control `word`, and make the write once the last one has
    /// completed — or a second after it, which §6.7.3.2 permits taking as
    /// completed. Answers when the reader should look again for a write still
    /// waiting.
    fn command(&self, word: Option<u16>, completed: bool) -> Option<u64> {
        let closed = crate::arch::IrqGuard::close();
        let mut command = self.command.lock(&closed);
        if word.is_some() {
            command.wanted = word;
        }
        let now = crate::clock::nanos_since_boot();
        if completed {
            command.since = None;
        }
        if let Some(since) = command.since {
            let due = since + hotplug::COMMAND_NANOS;
            if now < due {
                return command.wanted.map(|_| due - now);
            }
        }
        let word = command.wanted.take()?;
        self.pci.write_config_u16(self.at(hotplug::SLOT_CONTROL), word);
        command.since = self.slot.completes.then_some(now);
        None
    }

    /// After a claim behind this slot was released: power it off where that
    /// was the last thing it waited on. Nothing else, since the release runs
    /// on whatever thread let the claim go.
    pub(super) fn settle_released(&self) {
        if hotplug::next(self.seen()) == Next::PowerOff {
            self.command(Some(self.slot.control(false)), false);
        }
    }

    /// Act on what this slot is owed; answers how long until it may change
    /// with no interrupt to say so.
    fn refresh(&self) -> Option<u64> {
        let seen = self.seen();
        if !seen.occupied {
            self.removing.store(false, Ordering::Release);
        }
        let now = crate::clock::nanos_since_boot();
        let mut settle = None;
        if !(seen.occupied && seen.powered && seen.link_up) {
            self.link_since.store(0, Ordering::Release);
        }
        match hotplug::next(seen) {
            Next::Nothing => {}
            Next::Depart => self.depart(),
            Next::PowerOn => settle = self.command(Some(self.slot.control(true)), false),
            Next::PowerOff => settle = self.command(Some(self.slot.control(false)), false),
            Next::Enumerate => {
                let since = match self.link_since.load(Ordering::Acquire) {
                    0 => {
                        self.link_since.store(now, Ordering::Release);
                        now
                    }
                    since => since,
                };
                let ready = since + hotplug::READY_NANOS;
                if now < ready {
                    settle = Some(ready - now);
                } else {
                    self.enumerate();
                }
            }
        }
        for number in 0..super::RECORDED.load(Ordering::Acquire) {
            let function = function(number);
            if !self.behind(function) {
                continue;
            }
            let left = function.left.swap(0, Ordering::AcqRel);
            if left != 0 {
                log!("pcidev: {function} left the device set; arrival {left} is retired");
            }
            super::retire(number);
        }
        settle.or_else(|| self.command(None, false))
    }

    /// Read the functions below the slot into the device set, each a new
    /// arrival: on the record its requester had, or on a new one.
    fn enumerate(&self) {
        let bus = self.secondary;
        let Some(window) = crate::drivers::pci::function_window(bus, 0, 0) else {
            log!("pcidev: bus {bus:#04x} below a hot-plug slot is in no ECAM window, so nothing there is read");
            return;
        };
        let first = PciDevice::at(window, bus, 0, 0);
        if first.vendor_id() == crate::drivers::pci::INVALID_VENDOR {
            return;
        }
        let functions = if first.multi_function() { 8 } else { 1 };
        for func in 0..functions {
            let Some(window) = crate::drivers::pci::function_window(bus, 0, func) else { continue };
            let pci = PciDevice::at(window, bus, 0, func);
            if pci.vendor_id() == crate::drivers::pci::INVALID_VENDOR {
                continue;
            }
            if pci.bridge().is_some() {
                log!(
                    "pcidev: PCI {bus:02x}:00.{func} is a bridge, which needs bus numbers below it that \
                     this kernel does not assign, so nothing behind it is read"
                );
                continue;
            }
            arrive(pci);
        }
    }
}

/// `pci` in the device set as a new arrival.
fn arrive(pci: PciDevice) {
    let mut machine = MACHINE.lock();
    let arrival = Arrival(machine.next_arrival);
    let identity = pci.identity(arrival, super::addr_of(machine.segment, requester(&pci)));
    let number = match (0..super::RECORDED.load(Ordering::Acquire)).find(|n| function(*n).who == requester(&pci)) {
        Some(number) => {
            machine.identities[number] = identity;
            number
        }
        None => match record(&mut machine, super::new_record(pci, Arrival(0)), identity) {
            Some(number) => number,
            None => {
                log!(
                    "pcidev: PCI {:02x}:{:02x}.{} arrived past the {} functions this kernel keeps, and is not listed",
                    pci.bus,
                    pci.dev,
                    pci.func,
                    super::MAX_FUNCTIONS
                );
                return;
            }
        },
    };
    machine.next_arrival += 1;
    function(number).arrival.store(arrival.0, Ordering::Release);
    drop(machine);
    log!(
        "pcidev: PCI {:02x}:{:02x}.{} [{:04x}:{:04x}] arrived as arrival {}",
        pci.bus,
        pci.dev,
        pci.func,
        identity.vendor,
        identity.device,
        arrival.0
    );
    super::changed();
}
