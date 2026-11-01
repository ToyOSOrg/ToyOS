//! A PCI Express hot-plug slot (PCIe base spec 6.0 §6.7): which port has one,
//! the Slot Control word this kernel writes, what a slot in a given state is
//! owed next, and where an arrived function's BARs go inside its port's
//! windows ([`assign`]).
//!
//! **The slot's registers are read; what it is owed is decided here.** The
//! kernel's handler latches events and the device set's reader acts on
//! [`next`]; neither interprets a register bit itself.

/// Byte offsets from the PCI Express capability header (§7.5.3).
pub const LINK_CAPABILITIES: u64 = 0x0C;
pub const LINK_STATUS: u64 = 0x12;
pub const SLOT_CAPABILITIES: u64 = 0x14;
pub const SLOT_CONTROL: u64 = 0x18;
pub const SLOT_STATUS: u64 = 0x1A;

/// PCI Express Capabilities (§7.5.3.2): the Device/Port Type in bits 7:4, Slot
/// Implemented, and the Interrupt Message Number in bits 13:9 — the MSI-X
/// entry, or MSI message, hot-plug events are signalled with.
const PORT_TYPE_SHIFT: u16 = 4;
const ROOT_PORT: u16 = 0x4;
const DOWNSTREAM_PORT: u16 = 0x6;
const SLOT_IMPLEMENTED: u16 = 1 << 8;
const MESSAGE_SHIFT: u16 = 9;

/// Link Capabilities bit 20, Data Link Layer Link Active Reporting Capable
/// (§7.5.3.6), and Link Status bit 13, Data Link Layer Link Active (§7.5.3.8).
const LINK_ACTIVE_REPORTING: u32 = 1 << 20;
const LINK_ACTIVE: u16 = 1 << 13;

/// Slot Capabilities (§7.5.3.9).
const ATTENTION_BUTTON: u32 = 1 << 0;
const POWER_CONTROLLER: u32 = 1 << 1;
const ATTENTION_INDICATOR: u32 = 1 << 3;
const POWER_INDICATOR: u32 = 1 << 4;
const HOT_PLUG_CAPABLE: u32 = 1 << 6;
const NO_COMMAND_COMPLETED: u32 = 1 << 18;

/// Slot Control (§7.5.3.10).
const BUTTON_ENABLE: u16 = 1 << 0;
const PRESENCE_ENABLE: u16 = 1 << 3;
const COMMAND_ENABLE: u16 = 1 << 4;
const INTERRUPT_ENABLE: u16 = 1 << 5;
const ATTENTION_OFF: u16 = 0b11 << 6;
const POWER_INDICATOR_ON: u16 = 0b01 << 8;
const POWER_INDICATOR_OFF: u16 = 0b11 << 8;
/// Power Controller Control: 1 is off.
const POWER_OFF: u16 = 1 << 10;
const LINK_CHANGE_ENABLE: u16 = 1 << 12;

/// Slot Status (§7.5.3.11): the event bits, each write-1-to-clear, and
/// Presence Detect State.
pub const BUTTON_PRESSED: u16 = 1 << 0;
const POWER_FAULT: u16 = 1 << 1;
const LATCH_CHANGED: u16 = 1 << 2;
pub const PRESENCE_CHANGED: u16 = 1 << 3;
pub const COMMAND_COMPLETED: u16 = 1 << 4;
const PRESENT: u16 = 1 << 6;
pub const LINK_CHANGED: u16 = 1 << 8;
pub const EVENTS: u16 = BUTTON_PRESSED | POWER_FAULT | LATCH_CHANGED | PRESENCE_CHANGED | COMMAND_COMPLETED | LINK_CHANGED;

/// §6.6.1: software waits 100 ms after a link below a port trains (or, at 5.0
/// GT/s and below, after the device's reset ends) before its first
/// configuration request to the device.
pub const READY_NANOS: u64 = 100_000_000;

/// §6.7.3.2: a slot that has not signalled Command Completed a second after a
/// write is permitted to be taken as done.
pub const COMMAND_NANOS: u64 = 1_000_000_000;

/// A hot-plug slot's capabilities, as its port publishes them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Slot {
    pub button: bool,
    pub power_controller: bool,
    pub attention_indicator: bool,
    pub power_indicator: bool,
    /// Whether a Slot Control write is answered with Command Completed.
    pub completes: bool,
    /// Whether the port reports its link active.
    pub reports_link: bool,
    /// The MSI-X entry, or MSI message, its events arrive on.
    pub message: u8,
}

/// The slot a port with these registers has: `None` for a function that is
/// no root or downstream port, a port with no slot, and a slot that is not
/// hot-plug capable.
pub fn slot(capabilities: u16, link_capabilities: u32, slot_capabilities: u32) -> Option<Slot> {
    let port = (capabilities >> PORT_TYPE_SHIFT) & 0xF;
    let ported = port == ROOT_PORT || port == DOWNSTREAM_PORT;
    let hot = slot_capabilities & HOT_PLUG_CAPABLE != 0;
    (ported && capabilities & SLOT_IMPLEMENTED != 0 && hot).then_some(Slot {
        button: slot_capabilities & ATTENTION_BUTTON != 0,
        power_controller: slot_capabilities & POWER_CONTROLLER != 0,
        attention_indicator: slot_capabilities & ATTENTION_INDICATOR != 0,
        power_indicator: slot_capabilities & POWER_INDICATOR != 0,
        completes: slot_capabilities & NO_COMMAND_COMPLETED == 0,
        reports_link: link_capabilities & LINK_ACTIVE_REPORTING != 0,
        message: ((capabilities >> MESSAGE_SHIFT) & 0x1F) as u8,
    })
}

impl Slot {
    /// The Slot Control word for a slot powered or not: every event this
    /// slot has enabled, the power controller and power indicator set to
    /// match, and the attention indicator off. Only fields the slot has are
    /// written; the rest are reserved where it has nothing behind them.
    pub fn control(&self, powered: bool) -> u16 {
        let mut word = PRESENCE_ENABLE | INTERRUPT_ENABLE;
        if self.button {
            word |= BUTTON_ENABLE;
        }
        if self.completes {
            word |= COMMAND_ENABLE;
        }
        if self.reports_link {
            word |= LINK_CHANGE_ENABLE;
        }
        if self.attention_indicator {
            word |= ATTENTION_OFF;
        }
        if self.power_indicator {
            word |= if powered { POWER_INDICATOR_ON } else { POWER_INDICATOR_OFF };
        }
        if self.power_controller && !powered {
            word |= POWER_OFF;
        }
        word
    }

    /// Whether the slot is powered, by the Slot Control word it holds: one
    /// with no power controller always is.
    pub fn powered(&self, control: u16) -> bool {
        !self.power_controller || control & POWER_OFF == 0
    }

    /// Whether the link below has come up, from Link Status: a port that does
    /// not report it is taken at its word once the slot is powered and
    /// occupied, and §6.6.1's wait is then counted from there.
    pub fn link_up(&self, link_status: u16) -> bool {
        !self.reports_link || link_status & LINK_ACTIVE != 0
    }
}

/// Whether Slot Status says a card is in the slot.
pub fn occupied(status: u16) -> bool {
    status & PRESENT != 0
}

/// A slot's state, as the reader of the device set sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Seen {
    pub occupied: bool,
    pub powered: bool,
    pub link_up: bool,
    /// Its functions are in the device set.
    pub listed: bool,
    /// A claim still holds a function behind it, of this arrival or the last.
    pub held: bool,
    /// Its attention button asked for the card's removal, and the card has
    /// not left yet.
    pub removing: bool,
}

/// What a slot is owed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Next {
    Nothing,
    /// Its functions leave the set: every claim on them answers `Gone`.
    Depart,
    PowerOn,
    PowerOff,
    /// Read the functions below it, once §6.6.1's wait has passed.
    Enumerate,
}

/// What a slot in state `seen` is owed. A slot a claim still holds is never
/// powered either way and never enumerated: its last holder's release comes
/// first.
pub fn next(seen: Seen) -> Next {
    if seen.listed && (!seen.occupied || seen.removing) {
        return Next::Depart;
    }
    if seen.held {
        return Next::Nothing;
    }
    match (seen.occupied && !seen.removing, seen.powered) {
        (false, true) => Next::PowerOff,
        (false, false) => Next::Nothing,
        (true, false) => Next::PowerOn,
        (true, true) if seen.listed || !seen.link_up => Next::Nothing,
        (true, true) => Next::Enumerate,
    }
}

/// What a press of the attention button asks of a slot in state `seen`:
/// removal of a card whose functions are listed, and nothing otherwise — a
/// press on a slot this kernel has not powered and read is not taken as a
/// request to undo what was never done.
pub fn pressed_removes(seen: Seen) -> bool {
    seen.listed && seen.powered
}

/// The Type 1 header's bus numbers and memory windows (PCIe base spec 6.0
/// §7.5.1.3): the dword at 0x18, which holds Secondary and Subordinate Bus
/// Number in its second and third bytes.
pub const BUS_NUMBERS: u64 = 0x18;
/// Memory Base and Limit, one dword.
pub const MEMORY_WINDOW: u64 = 0x20;
/// Prefetchable Memory Base and Limit, one dword, and their upper 32 bits.
pub const PREFETCHABLE_WINDOW: u64 = 0x24;
pub const PREFETCHABLE_BASE_UPPER: u64 = 0x28;
pub const PREFETCHABLE_LIMIT_UPPER: u64 = 0x2C;

/// A window's granularity: bits 15:4 of a base or limit word are address
/// bits 31:20, and a limit's bits 19:0 are all ones.
const WINDOW_GRANULE: u64 = 1 << 20;

/// A port's secondary and subordinate bus numbers, from [`BUS_NUMBERS`].
pub fn buses(dword: u32) -> (u8, u8) {
    ((dword >> 8) as u8, (dword >> 16) as u8)
}

/// The memory a port forwards below it, each window as `(start, end)` with
/// `end` exclusive; `None` for a window whose base is above its limit, which
/// forwards nothing, or whose limit is the last megabyte of the address
/// space, which no exclusive end can name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Windows {
    pub memory: Option<(u64, u64)>,
    pub prefetchable: Option<(u64, u64)>,
}

impl Windows {
    /// From the dwords at [`MEMORY_WINDOW`], [`PREFETCHABLE_WINDOW`],
    /// [`PREFETCHABLE_BASE_UPPER`] and [`PREFETCHABLE_LIMIT_UPPER`]. The upper
    /// halves count only where the prefetchable window says it is 64-bit
    /// (bits 3:0 of its base word, 1).
    pub fn read(memory: u32, prefetchable: u32, base_upper: u32, limit_upper: u32) -> Self {
        let window = |base: u64, limit: u64| match base <= limit {
            true => limit.checked_add(WINDOW_GRANULE).map(|end| (base, end)),
            false => None,
        };
        let low = |word: u32| u64::from(word & 0xFFF0) << 16;
        let wide = prefetchable & 0xF == 1;
        let upper = |word: u32| if wide { u64::from(word) << 32 } else { 0 };
        Self {
            memory: window(low(memory), low(memory >> 16)),
            prefetchable: window(low(prefetchable) | upper(base_upper), low(prefetchable >> 16) | upper(limit_upper)),
        }
    }
}

/// Where a BAR of `kind` and `size` bytes goes below a port with `windows`:
/// the lowest address in the first window that may hold it, aligned to its
/// size and to a 4 KiB page, whose pages no range of `taken` touches, so it
/// is handed over in pages of its own. A prefetchable BAR tries the
/// prefetchable window first and then the memory window, which may hold
/// either kind; a 32-bit BAR ends below 4 GiB. `None` where no window has
/// room.
pub fn assign(kind: crate::bar::Kind, size: u64, windows: Windows, taken: &[(u64, u64)]) -> Option<u64> {
    if size == 0 || !size.is_power_of_two() {
        return None;
    }
    let span = size.max(crate::pages::PAGE);
    let ceiling = if kind.wide { u64::MAX } else { 1 << 32 };
    let tried = [kind.prefetchable.then_some(windows.prefetchable).flatten(), windows.memory];
    tried.into_iter().flatten().find_map(|(start, end)| lowest(span, start, end.min(ceiling), taken))
}

/// The lowest `span`-aligned `at` with `at..at + span` inside `start..end`
/// and clear of every range of `taken`.
fn lowest(span: u64, start: u64, end: u64, taken: &[(u64, u64)]) -> Option<u64> {
    let page = |at: u64| at & !(crate::pages::PAGE - 1);
    let mut at = start.checked_next_multiple_of(span)?;
    loop {
        let stop = at.checked_add(span)?;
        if stop > end {
            return None;
        }
        // A range's pages, not its bytes: a BAR sharing a page is handed to nobody.
        let clash = taken.iter().filter(|(from, to)| page(*from) < stop && at < *to).map(|(_, to)| *to).max();
        match clash {
            None => return Some(at),
            Some(past) => at = past.checked_next_multiple_of(span)?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// QEMU 11.1.1's `pcie-root-port`, as read off its registers: a root port
    /// with a slot, its message 0, and a slot with a button, a power
    /// controller, both indicators and command completion, whose link reports
    /// itself active.
    const QEMU_CAPS: u16 = 0x0142;
    const QEMU_LINK: u32 = 0x0030_0604;
    const QEMU_SLOT: u32 = 0x001a_007b;

    fn qemu() -> Slot {
        slot(QEMU_CAPS, QEMU_LINK, QEMU_SLOT).expect("a hot-plug slot")
    }

    #[test]
    fn a_root_port_with_a_hot_plug_slot_is_read_whole() {
        assert_eq!(
            qemu(),
            Slot {
                button: true,
                power_controller: true,
                attention_indicator: true,
                power_indicator: true,
                completes: true,
                reports_link: true,
                message: 0,
            }
        );
        // The message number is bits 13:9.
        assert_eq!(slot(QEMU_CAPS | (0x1F << 9), QEMU_LINK, QEMU_SLOT).map(|s| s.message), Some(0x1F));
    }

    #[test]
    fn only_a_hot_plug_capable_slot_on_a_port_is_one() {
        assert_eq!(slot(QEMU_CAPS, QEMU_LINK, QEMU_SLOT & !HOT_PLUG_CAPABLE), None);
        assert_eq!(slot(QEMU_CAPS & !SLOT_IMPLEMENTED, QEMU_LINK, QEMU_SLOT), None);
        // An endpoint (0), an upstream port (5) and a root complex event
        // collector (0xA) are no ports a slot hangs off.
        for port in [0x0, 0x5, 0xA] {
            assert_eq!(slot((QEMU_CAPS & !0xF0) | (port << 4), QEMU_LINK, QEMU_SLOT), None, "type {port:#x}");
        }
        assert!(slot((QEMU_CAPS & !0xF0) | (DOWNSTREAM_PORT << 4), QEMU_LINK, QEMU_SLOT).is_some());
    }

    /// Every enable the slot has, the attention indicator off, and power and
    /// its indicator together; a slot without a power controller or
    /// indicators is written none of them.
    #[test]
    fn slot_control_holds_exactly_the_fields_the_slot_has() {
        let slot = qemu();
        let on = slot.control(true);
        assert_eq!(on, 0x11f9);
        assert_eq!(slot.control(false), 0x17f9);
        assert!(slot.powered(on));
        assert!(!slot.powered(slot.control(false)));
        let bare = Slot { button: false, power_controller: false, attention_indicator: false, power_indicator: false, completes: false, reports_link: false, message: 0 };
        assert_eq!(bare.control(false), PRESENCE_ENABLE | INTERRUPT_ENABLE);
        assert!(bare.powered(bare.control(false)), "a slot with no power controller is always powered");
        assert!(bare.link_up(0), "a port that reports no link is taken as up");
        assert!(!slot.link_up(0x0204) && slot.link_up(0x2204));
    }

    fn seen() -> Seen {
        Seen { occupied: true, powered: true, link_up: true, ..Seen::default() }
    }

    /// An arrival: powered on, then read once its link is up and nobody
    /// holds what was there before.
    #[test]
    fn a_card_is_powered_then_read() {
        assert_eq!(next(Seen { powered: false, link_up: false, ..seen() }), Next::PowerOn);
        assert_eq!(next(Seen { link_up: false, ..seen() }), Next::Nothing);
        assert_eq!(next(seen()), Next::Enumerate);
        assert_eq!(next(Seen { listed: true, ..seen() }), Next::Nothing);
        // The last arrival's holder has not let go: the new one waits.
        assert_eq!(next(Seen { held: true, ..seen() }), Next::Nothing);
        assert_eq!(next(Seen { held: true, powered: false, ..seen() }), Next::Nothing);
    }

    /// A removal: listed functions leave first, the slot is powered off once
    /// no claim holds one, and the empty slot stays off.
    #[test]
    fn a_card_leaves_then_its_slot_goes_off() {
        assert_eq!(next(Seen { listed: true, removing: true, ..seen() }), Next::Depart);
        assert_eq!(next(Seen { listed: true, occupied: false, ..seen() }), Next::Depart);
        assert_eq!(next(Seen { removing: true, held: true, ..seen() }), Next::Nothing);
        assert_eq!(next(Seen { removing: true, ..seen() }), Next::PowerOff);
        assert_eq!(next(Seen { occupied: false, ..seen() }), Next::PowerOff);
        assert_eq!(next(Seen { occupied: false, powered: false, ..seen() }), Next::Nothing);
        assert_eq!(next(Seen { removing: true, powered: false, ..seen() }), Next::Nothing);
    }

    #[test]
    fn a_press_removes_only_what_was_powered_and_read() {
        assert!(pressed_removes(Seen { listed: true, ..seen() }));
        assert!(!pressed_removes(seen()));
        assert!(!pressed_removes(Seen { listed: true, powered: false, ..seen() }));
    }

    #[test]
    fn presence_is_bit_six_and_every_event_is_cleared() {
        assert!(occupied(0x0049) && occupied(0x0059) && !occupied(0x0019));
        assert_eq!(EVENTS, 0x011F);
    }

    /// QEMU 11.1.1's `pcie-root-port` below OVMF, as its registers read: a
    /// 2 MiB memory window and a 64-bit prefetchable one.
    const QEMU_MEMORY: u32 = 0xc1e0_c1e0;
    const QEMU_PREFETCHABLE: u32 = 0x1ff1_0001;

    #[test]
    fn a_ports_windows_are_read_from_its_bridge_registers() {
        assert_eq!(buses(0x0001_0100), (1, 1));
        let windows = Windows::read(QEMU_MEMORY, QEMU_PREFETCHABLE, 0x8, 0x8);
        assert_eq!(windows.memory, Some((0xc1e0_0000, 0xc1f0_0000)));
        assert_eq!(windows.prefetchable, Some((0x8_0000_0000, 0x8_2000_0000)));
        // A 32-bit prefetchable window ignores what its upper registers hold,
        // and a base past its limit forwards nothing.
        assert_eq!(Windows::read(QEMU_MEMORY, 0x1ff0_0000, 0x8, 0x8).prefetchable, Some((0, 0x2000_0000)));
        assert_eq!(Windows::read(0x0000_fff0, 0x0000_fff1, 0, 0), Windows { memory: None, prefetchable: None });
        // A 64-bit limit in the address space's last megabyte has no
        // exclusive end, and is refused rather than wrapped; the megabyte
        // below it is a window like any other.
        let top = Windows::read(QEMU_MEMORY, 0xfff0_0001, 0xffff_ffff, 0xffff_ffff);
        assert_eq!(top.prefetchable, None);
        let below = Windows::read(QEMU_MEMORY, 0xffe0_ffe1, 0xffff_ffff, 0xffff_ffff);
        assert_eq!(below.prefetchable, Some((0xffff_ffff_ffe0_0000, 0xffff_ffff_fff0_0000)));
    }

    fn qemu_windows() -> Windows {
        Windows::read(QEMU_MEMORY, QEMU_PREFETCHABLE, 0x8, 0x8)
    }

    const NARROW: crate::bar::Kind = crate::bar::Kind { wide: false, prefetchable: false };
    const WIDE_PREFETCHABLE: crate::bar::Kind = crate::bar::Kind { wide: true, prefetchable: true };

    /// The lowest aligned address in the window the BAR's kind takes.
    #[test]
    fn a_bar_goes_at_the_bottom_of_its_window() {
        // `edu`'s BAR 0: 1 MiB, 32-bit, not prefetchable.
        assert_eq!(assign(NARROW, 0x10_0000, qemu_windows(), &[]), Some(0xc1e0_0000));
        assert_eq!(assign(WIDE_PREFETCHABLE, 0x4000, qemu_windows(), &[]), Some(0x8_0000_0000));
        // Smaller than a page, it still takes a page of its own.
        assert_eq!(assign(NARROW, 0x10, qemu_windows(), &[(0xc1e0_0000, 0xc1e0_0010)]), Some(0xc1e0_1000));
    }

    /// Past every range something already decodes in, by whole pages and at
    /// the BAR's own alignment.
    #[test]
    fn a_bar_is_placed_clear_of_what_is_taken() {
        let taken = [(0xc1e0_0000, 0xc1e0_1000), (0xc1e4_0000, 0xc1e4_0004)];
        assert_eq!(assign(NARROW, 0x8_0000, qemu_windows(), &taken), Some(0xc1f0_0000 - 0x8_0000));
        assert_eq!(assign(NARROW, 0x10_0000, qemu_windows(), &taken), None, "the window's one MiB is taken in part");
        // A range ending inside the page the candidate starts in pushes it on.
        assert_eq!(assign(NARROW, 0x1000, qemu_windows(), &[(0xc1e0_0000, 0xc1e0_0800)]), Some(0xc1e0_1000));
    }

    /// A prefetchable BAR the prefetchable window cannot hold goes to the
    /// memory window; a 32-bit one never goes above 4 GiB; a size that is no
    /// power of two is no BAR.
    #[test]
    fn a_bar_takes_only_a_window_that_may_hold_it() {
        let narrow_prefetchable = crate::bar::Kind { wide: false, prefetchable: true };
        assert_eq!(assign(narrow_prefetchable, 0x1000, qemu_windows(), &[]), Some(0xc1e0_0000));
        assert_eq!(assign(NARROW, 0x1000, Windows { memory: None, ..qemu_windows() }, &[]), None);
        assert_eq!(assign(WIDE_PREFETCHABLE, 0x4000_0000, qemu_windows(), &[]), None);
        assert_eq!(assign(NARROW, 0x3000, qemu_windows(), &[]), None);
        assert_eq!(assign(NARROW, 0, qemu_windows(), &[]), None);
        let top = Windows { memory: Some((u64::MAX - 0xFFFFF, u64::MAX)), prefetchable: None };
        assert_eq!(assign(WIDE_PREFETCHABLE, 0x10_0000, top, &[]), None, "no wrap past the top of the address space");
    }
}
