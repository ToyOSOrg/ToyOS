//! The hot-plug slots this kernel drives: a PCI Express port's slot (PCIe base
//! spec 6.0 §6.7), armed only below a root bridge whose `_OSC` commit granted
//! the kernel native hot-plug and PCI Express capability structure control
//! (ACPI 6.5 §6.2.11; PCI Firmware Specification 3.3 §4.5.1), as the `acpi`
//! claim's holder declared it.
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
//! **An arrived function's BARs are assigned once, before it is listed**: each
//! one the device leaves unassigned is given the lowest address inside its
//! port's windows that no other function decodes in
//! (`toyos_pci::hotplug::assign`) and that lies inside the declared root
//! bridges' memory, and is kept only where the register then reads it back.
//! Nothing reads through the BAR to prove it; the port's windows are
//! firmware's, inside its root's.
//!
//! **A slot is armed only where the unit blocks every requester behind it
//! it has not seen, and once every function enumerated behind it at boot is
//! on a domain of its own** ([`unarmable`], [`isolate_behind`]): a card
//! seated behind it then reaches nothing until a claim grants it memory,
//! but, on VT-d, the regions firmware reserved for that requester.
//!
//! **Removal is the button's or the card's**: a press on a powered slot whose
//! functions are listed removes them at once, and the slot is powered off when
//! the last claim on them is released. §6.7.1.5's five seconds in which a
//! second press cancels the removal are not offered.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use toyos_abi::acpi::{RootBridgeWindow, CAPABILITY_STRUCTURE, NATIVE_HOT_PLUG};
use toyos_abi::inventory::Arrival;
use toyos_pci::hotplug::{self, Next, Seen};
use toyos_pci::{aperture, bar};

use super::{function, functions, record, requester, Armed, Decoded, Function, Refusal, RootBridge, MACHINE};
use crate::drivers::pci::{arm_claimed_msi, arm_claimed_msix, NoCapability, NoEntry, PciDevice, MSIX_ENTRY};
use crate::sync::{Lock, Masked};

/// Every slot armed, for the reader to walk.
static SLOTS: Lock<Vec<&'static HotPlug>> = Lock::new(Vec::new());

/// Held across a [`refresh`], so two readers of the set never enumerate one
/// arrival twice: a spinlock over at most one enumeration, eight functions'
/// BAR sizing, which a second reader waits out.
static REFRESHING: Lock<()> = Lock::new(());

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

/// Arm the slot of every port below one of `roots` whose `_OSC` commit was
/// granted both controls, and say for every port why its slot is or is not
/// armed. On the declarer's thread, before the declaration of `windows` is
/// published, so no claim runs on a function behind a slot meanwhile.
pub(super) fn arm(roots: &[RootBridge], windows: &[RootBridgeWindow]) {
    let (ports, segment): (Vec<super::Port>, u16) = {
        let machine = MACHINE.lock();
        (machine.ports.iter().filter(|port| !port.armed).copied().collect(), machine.segment)
    };
    for super::Port { number, express, slot, buses, .. } in ports {
        let port = function(number);
        let bus = port.pci.bus;
        let declared = roots.iter().find(|root| root.segment == segment && (root.buses.0..=root.buses.1).contains(&bus));
        let granted = match declared.map(|root| root.committed) {
            None => {
                log!("pcidev: {port}'s hot-plug slot is not armed: no root bridge declared covers bus {bus:#04x}");
                continue;
            }
            Some(None) => {
                log!("pcidev: {port}'s hot-plug slot is not armed: its root bridge's _OSC committed nothing");
                continue;
            }
            Some(Some(granted)) => granted,
        };
        let needed = NATIVE_HOT_PLUG | CAPABILITY_STRUCTURE;
        if granted & needed != needed {
            log!(
                "pcidev: {port}'s hot-plug slot is not armed: its root bridge's _OSC commit was granted \
                 {granted:#x}, without native hot-plug and capability structure control ({needed:#x})"
            );
            continue;
        }
        let remapping = match unarmable(port, slot, buses) {
            Ok(remapping) => remapping,
            Err(why) => {
                log!("pcidev: {port}'s hot-plug slot is not armed: {why}");
                continue;
            }
        };
        if port.held.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            log!("pcidev: {port}'s hot-plug slot is not armed: a claim holds the port");
            continue;
        }
        let armed = isolate_behind(buses, windows).and_then(|()| arm_one(number, port, express, slot, buses, remapping));
        match armed {
            Ok(armed) => {
                {
                    // The port is this kernel's now: the set lists it so, and
                    // a claim on it is refused as one on a kernel driver's.
                    let mut machine = MACHINE.lock();
                    machine.ports.iter_mut().filter(|p| p.number == number).for_each(|p| p.armed = true);
                    if !machine.kernel_driven.contains(&port.who) {
                        machine.kernel_driven.push(port.who);
                    }
                }
                SLOTS.lock().push(armed);
                log!("pcidev: {port}'s hot-plug slot is armed, and the port is this kernel's: its root bridge's _OSC commit was granted {granted:#x}");
                armed.command(Some(slot.control(armed.seen().occupied && armed.powered())), false);
                super::changed();
            }
            Err(why) => {
                port.held.store(false, Ordering::Release);
                log!("pcidev: {port}'s hot-plug slot is not armed: {why}");
            }
        }
    }
}

/// Why `port`'s `slot`, forwarding `buses`, may not be armed, asked before
/// anything is changed: its message, whether the machine remaps the port's
/// and isolates it, whether the unit blocks every requester on `buses` it
/// has not seen, and whether each function behind it at boot can be given a
/// domain of its own.
fn unarmable(port: &Function, slot: hotplug::Slot, buses: (u8, u8)) -> Result<crate::iommu::Remapping, String> {
    if usize::from(slot.message) != usize::from(MSIX_ENTRY) {
        return Err("its slot signals on a message other than the one entry this kernel arms".into());
    }
    let remapping = crate::iommu::remapping().map_err(|why| Refusal::NotRemapped(why).to_string())?;
    crate::iommu::isolates(&port.pci).map_err(|why| Refusal::NotIsolated(why).to_string())?;
    crate::iommu::governs(buses).map_err(|why| alloc::format!("buses {:#04x}..={:#04x} below it: {why}", buses.0, buses.1))?;
    let kernel_driven = MACHINE.lock().kernel_driven.clone();
    for function in functions().filter(|function| (buses.0..=buses.1).contains(&function.pci.bus)) {
        if kernel_driven.contains(&function.who) {
            return Err(alloc::format!("{function} behind it is driven by this kernel"));
        }
        crate::iommu::isolates(&function.pci).map_err(|why| alloc::format!("{function} behind it is given no domain of its own: {why}"))?;
    }
    Ok(remapping)
}

/// Put every function enumerated at boot on `buses` on a domain of its own,
/// clear of the declared `windows`, which maps nothing a claim did not grant
/// and, on VT-d, the regions firmware reserved for it: VT-d and AMD-Vi gave
/// each the identity domain at boot, which a card seated at its requester
/// later would reach all of memory through. No claim holds one yet, since
/// the declaration is not published.
fn isolate_behind(buses: (u8, u8), windows: &[RootBridgeWindow]) -> Result<(), String> {
    for number in 0..super::RECORDED.load(Ordering::Acquire) {
        let function = function(number);
        if !(buses.0..=buses.1).contains(&function.pci.bus) {
            continue;
        }
        let space = super::function_space(number, windows)
            .map_err(|why| alloc::format!("{function} behind it is given no domain of its own: {why}"))?;
        space.space.attach(function.pci.bus, function.pci.dev, function.pci.func);
    }
    Ok(())
}

/// Arm `port`'s slot interrupt, on the message a claim's would be: remapped
/// for the port alone. `port` is held for this kernel.
fn arm_one(
    number: usize,
    port: &'static Function,
    express: u64,
    slot: hotplug::Slot,
    buses: (u8, u8),
    remapping: crate::iommu::Remapping,
) -> Result<&'static HotPlug, String> {
    let pci = port.pci;
    let message = crate::iommu::claim_msi(remapping, number, &pci).map_err(|why| Refusal::NoMessage(why).to_string())?;
    pci.enable_memory_space();
    let armed = match arm_claimed_msix(&message) {
        Ok(at) => Armed::Msix(at, message),
        Err(NoEntry::NoTable(NoCapability::Absent)) if arm_claimed_msi(&message) => Armed::Msi(message),
        Err(_) => return Err(Refusal::NoInterrupt.to_string()),
    };
    // A port's own message is a write it masters (PCI 3.0 §6.2.2, bit 2),
    // and what arrives behind it reaches memory through it.
    pci.enable_bus_master();
    let armed: &'static HotPlug = alloc::boxed::Box::leak(alloc::boxed::Box::new(HotPlug {
        pci,
        express,
        slot,
        secondary: buses.0,
        subordinate: buses.1,
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
    let _refreshing = REFRESHING.lock();
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
        self.link_since.store(0, Ordering::Release);
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
        // A card pulled and another seated between two reads is a new link,
        // owed §6.6.1's wait from its own training, not the last card's.
        if events & hotplug::PRESENCE_CHANGED != 0 {
            self.link_since.store(0, Ordering::Release);
        }
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
            log!(
                "pcidev: PCI {:02x}:{:02x}.{}'s slot is occupied and powered with its link up past \
                 §6.6.1's 100 ms, and {bus:02x}:00.0 answers no vendor, so this read lists nothing \
                 below it",
                self.pci.bus, self.pci.dev, self.pci.func
            );
            return;
        }
        let functions = if first.multi_function() { 8 } else { 1 };
        for func in 0..functions {
            let Some(window) = crate::drivers::pci::function_window(bus, 0, func) else { continue };
            let pci = PciDevice::at(window, bus, 0, func);
            if pci.vendor_id() == crate::drivers::pci::INVALID_VENDOR {
                continue;
            }
            if pci.header_type() != 0 {
                log!(
                    "pcidev: PCI {bus:02x}:00.{func} is a bridge, which needs bus numbers below it that \
                     this kernel does not assign, so nothing behind it is read"
                );
                continue;
            }
            self.arrive(pci);
        }
    }

    /// `pci` in the device set as a new arrival, its BARs assigned first.
    fn arrive(&self, pci: PciDevice) {
        let (declared, taken): (Vec<RootBridgeWindow>, Vec<(u64, u64)>) = {
            let machine = MACHINE.lock();
            let declared = match machine.apertures.answer() {
                Some(Ok(declaration)) => declaration.windows().to_vec(),
                _ => unreachable!("pcidev: a slot is armed only once the root bridges' memory is held"),
            };
            let who = requester(&pci);
            let taken = machine.decoded.iter().filter(|bar| bar.who != who).map(|bar| {
                let (_, start, end) = bar.extent();
                (start, end)
            });
            (declared, taken.collect())
        };
        let bars = self.assign(&pci, &declared, taken);
        arrive(pci, bars);
    }

    /// Size every memory BAR of `pci`, which nothing drives yet, and give
    /// each one it leaves unassigned an address ([`hotplug::assign`]) clear of
    /// `taken`: what it decodes then, for [`super::Machine::decoded`].
    fn assign(&self, pci: &PciDevice, declared: &[RootBridgeWindow], mut taken: Vec<(u64, u64)>) -> Vec<Decoded> {
        let windows = hotplug::Windows::read(
            self.pci.read_config_u32(hotplug::MEMORY_WINDOW),
            self.pci.read_config_u32(hotplug::PREFETCHABLE_WINDOW),
            self.pci.read_config_u32(hotplug::PREFETCHABLE_BASE_UPPER),
            self.pci.read_config_u32(hotplug::PREFETCHABLE_LIMIT_UPPER),
        );
        let who = requester(pci);
        let at = |index: u8| (pci.bus, pci.dev, pci.func, index);
        let mut decoded = Vec::new();
        pci.set_memory_decode(false);
        let mut index = 0u8;
        while index < pci.bar_slots() {
            let offset = bar::BASE + u64::from(index) * 4;
            let low = pci.read_config_u32(offset);
            let Ok(kind) = bar::kind(index, low) else {
                index += 1;
                continue;
            };
            let step = if kind.wide { 2 } else { 1 };
            let size = match pci.bar_size(index) {
                Ok(size) => size,
                Err(bar::BadSize::Unimplemented) => {
                    index += step;
                    continue;
                }
                Err(why) => {
                    let (b, d, f, i) = at(index);
                    log!("pcidev: PCI {b:02x}:{d:02x}.{f} BAR {i} answers no size it can be placed by ({why:?}), so it is given no address");
                    index += step;
                    continue;
                }
            };
            let base = match pci.memory_bar(index) {
                Ok(memory) => Some(memory.address()),
                Err(bar::Unusable::Unassigned) => self.place(pci, (index, low, kind, size), Room { windows, declared, taken: &taken }),
                Err(_) => None,
            };
            if let Some(base) = base {
                decoded.push(Decoded { who, index, base, bytes: Ok(size) });
                taken.push((base, base.saturating_add(size)));
            }
            index += step;
        }
        decoded
    }

    /// One address in `room` for BAR `index`, of `kind` and `size` bytes,
    /// whose low dword reads `low` and which its device left unassigned;
    /// written, and kept where the register reads it back.
    fn place(&self, pci: &PciDevice, (index, low, kind, size): (u8, u32, bar::Kind, u64), room: Room<'_>) -> Option<u64> {
        let Room { windows, declared, taken } = room;
        let (b, d, f) = (pci.bus, pci.dev, pci.func);
        let Some(at) = hotplug::assign(kind, size, windows, taken) else {
            log!("pcidev: PCI {b:02x}:{d:02x}.{f} BAR {index} ({size:#x} bytes) fits in no window of its port {windows:x?}, so it is given no address");
            return None;
        };
        if !matches!(aperture::decode(declared, at, at + size), aperture::Decode::Inside(_)) {
            log!("pcidev: PCI {b:02x}:{d:02x}.{f} BAR {index}'s place at {at:#x} in its port's window is inside no declared root bridge window, so it is given no address");
            return None;
        }
        let placed = bar::placement(index, low, at).ok()?;
        let offset = bar::BASE + u64::from(index) * 4;
        pci.write_config_u32(offset, placed.low);
        if let Some(high) = placed.high {
            pci.write_config_u32(offset + 4, high);
        }
        if !matches!(pci.memory_bar(index), Ok(memory) if memory.address() == at) {
            pci.write_config_u32(offset, low);
            if placed.high.is_some() {
                pci.write_config_u32(offset + 4, 0);
            }
            log!("pcidev: PCI {b:02x}:{d:02x}.{f} BAR {index} did not take {at:#x}, so it is given no address");
            return None;
        }
        log!("pcidev: PCI {b:02x}:{d:02x}.{f} BAR {index} ({size:#x} bytes) assigned {at:#x}, inside its port's window");
        Some(at)
    }
}

/// Where an arrived function's BAR may go: inside its port's `windows` and
/// the `declared` root bridges' memory, clear of what is `taken`.
#[derive(Clone, Copy)]
struct Room<'a> {
    windows: hotplug::Windows,
    declared: &'a [RootBridgeWindow],
    taken: &'a [(u64, u64)],
}

/// `pci` in the device set as a new arrival, decoding `bars`.
fn arrive(pci: PciDevice, bars: Vec<Decoded>) {
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
    // What a departed arrival on this record decoded, where the release of
    // its last claim let this enumeration in before its `retire` ran.
    machine.decoded.retain(|bar| bar.who != requester(&pci));
    machine.decoded.extend(bars);
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
