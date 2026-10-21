//! usbd's side of the `input` port: every bound keyboard and pointer, what
//! the owner of input was last told of each, and the connection that follows
//! them (`toyos_input::wire`).
//!
//! **The connection follows the devices.** It is opened when the first device
//! binds and dropped once the last one's `gone` is sent, so a controller that
//! carries only storage holds none of the owner's connections. While a device
//! is bound and there is none — the owner went, or never accepted — the next
//! report opens one, at most once a second ([`REOPEN_NS`]), and a report that
//! falls meanwhile is counted [`Counts::unowned`].
//!
//! **usbd never blocks on the owner, and keeps no queue.** Per device it keeps
//! what the owner was last told and the device's current state; what is sent
//! is the difference, record by record, each `try_send`, and a full connection
//! stops the sending until it takes writes again. So a release is never lost,
//! and a press and release that both fall while it is full are never
//! delivered: they are counted [`Counts::coalesced`].
//!
//! **A device's `source` is its number on this connection**, in bind order
//! from 0, never reused while the connection lasts: a late record never
//! reaches a later device. A fresh connection numbers the devices again.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use toyos_abi::input::{RawKeyEvent, MOD_RELEASED};
use toyos_input::keys::{Keys, Usages};
use toyos_input::wire::{Motion, Record, Report, DEVICES};
use toyos_usbhid::report::Pointer;

use crate::exec::Nanos;

/// How often a device bound with no connection tries the owner again.
pub const REOPEN_NS: Nanos = 1_000_000_000;

/// What the connection did with one record.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sent {
    Taken,
    /// It holds all it can: try again once it takes writes.
    Full,
    /// The owner hung up.
    Ended,
}

/// How an open of the owner's port went.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Opened {
    Opened,
    /// None could be made now.
    Refused,
    /// This usbd was not given the port: it serves no input device.
    NotGiven,
}

/// The owner of input, as usbd reaches it.
pub trait Owner {
    fn open(&mut self) -> Opened;
    fn send(&mut self, record: &Record) -> Sent;
    fn close(&mut self);
}

/// What a device was told, or is now.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct State {
    keys: Usages,
    buttons: u8,
    /// An absolute pointer's position.
    at: Option<(u16, u16)>,
    /// A relative pointer's motion and the wheel's, summed since told.
    dx: i32,
    dy: i32,
    scroll: i32,
}

struct Device {
    source: u16,
    told: State,
    now: State,
    /// Keys and buttons that moved since the owner was last told.
    touched: Usages,
    touched_buttons: u8,
    /// Its `gone` is owed.
    gone: bool,
}

/// What the link counted, for `inspect`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Counts {
    pub sent: u64,
    pub coalesced: u64,
    pub unowned: u64,
    pub opened: u64,
    pub refused: u64,
}

/// Every bound device and the connection they share.
#[derive(Default)]
pub struct Inputs {
    /// By the handle usbd's HID functions hold.
    devices: BTreeMap<u32, Device>,
    next_handle: u32,
    next_source: u32,
    connected: bool,
    /// Every number a connection may give out was given: the next flush
    /// drops it, and the one after numbers the devices afresh.
    spent: bool,
    full: bool,
    /// A report arrived with no connection since the last open was tried.
    asked: bool,
    last_open: Option<Nanos>,
    /// The port is not this usbd's: nothing is bound.
    not_given: bool,
    pub counts: Counts,
}

/// A bound device, as its HID function names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Handle(u32);

impl Inputs {
    /// A device bound: `None` once [`DEVICES`] are, which is what one
    /// connection may carry.
    pub fn bind(&mut self) -> Option<Handle> {
        if self.not_given || self.devices.values().filter(|d| !d.gone).count() >= DEVICES {
            return None;
        }
        let handle = self.next_handle;
        self.next_handle += 1;
        let source = self.source();
        self.devices.insert(handle, Device { source, told: State::default(), now: State::default(), touched: Usages::NONE, touched_buttons: 0, gone: false });
        self.asked = true;
        Some(Handle(handle))
    }

    fn source(&mut self) -> u16 {
        let source = self.next_source;
        self.next_source += 1;
        self.spent |= source > u32::from(u16::MAX);
        source as u16
    }


    fn reported(&mut self) {
        if !self.connected {
            self.counts.unowned += 1;
            self.asked = true;
        }
    }

    /// A keyboard now holds `keys`.
    pub fn keys(&mut self, handle: Handle, keys: Usages) {
        // A device unbound with the port that was found not to be this
        // usbd's says nothing.
        let Some(device) = self.devices.get_mut(&handle.0) else { return };
        device.touched = device.touched.union(Usages::symmetric(device.now.keys, keys));
        device.now.keys = keys;
        self.reported();
    }

    /// A pointer reported `report`.
    pub fn pointer(&mut self, handle: Handle, report: Pointer) {
        let Some(device) = self.devices.get_mut(&handle.0) else { return };
        device.touched_buttons |= device.now.buttons ^ report.buttons;
        device.now.buttons = report.buttons;
        device.now.scroll = device.now.scroll.saturating_add(i32::from(report.scroll));
        match report.motion {
            Motion::Absolute { x, y } => device.now.at = Some((x, y)),
            Motion::Relative { dx, dy } => {
                device.now.dx = device.now.dx.saturating_add(i32::from(dx));
                device.now.dy = device.now.dy.saturating_add(i32::from(dy));
            }
        }
        self.reported();
    }

    /// The device left: what it holds is released, and its `gone` is owed.
    pub fn gone(&mut self, handle: Handle) {
        if let Some(device) = self.devices.get_mut(&handle.0) {
            device.gone = true;
        }
    }

    /// Whether this usbd was found not to be given the owner's port.
    pub fn not_given(&self) -> bool {
        self.not_given
    }

    /// Whether the connection is full, and the caller should send again once
    /// it takes writes.
    pub fn full(&self) -> bool {
        self.connected && self.full
    }

    /// Tell the owner every difference it has not been told, opening or
    /// dropping the connection as the devices ask.
    pub fn flush(&mut self, owner: &mut impl Owner, now: Nanos) {
        if self.connected && self.spent {
            owner.close();
            self.connected = false;
            self.asked = true;
            self.last_open = None;
        }
        if !self.connected {
            if self.devices.values().all(|d| d.gone) {
                // Nobody bound: what the last connection owed it ended with it.
                self.devices.clear();
                return;
            }
            let due = self.last_open.is_none_or(|at| now >= at.saturating_add(REOPEN_NS));
            if !self.asked || !due {
                return;
            }
            self.asked = false;
            self.last_open = Some(now);
            match owner.open() {
                Opened::Opened => {}
                Opened::Refused => {
                    self.counts.refused += 1;
                    return;
                }
                Opened::NotGiven => {
                    self.not_given = true;
                    self.devices.clear();
                    return;
                }
            }
            self.counts.opened += 1;
            self.connected = true;
            self.full = false;
            // A fresh owner knows nothing: every device is numbered again and
            // told its whole state.
            self.devices.retain(|_, d| !d.gone);
            self.next_source = 0;
            self.spent = false;
            let handles: Vec<u32> = self.devices.keys().copied().collect();
            for handle in handles {
                let source = self.source();
                let d = self.devices.get_mut(&handle).expect("listed");
                d.source = source;
                d.told = State::default();
                d.touched = Usages::NONE;
                d.touched_buttons = 0;
            }
        }
        self.full = false;
        let handles: Vec<u32> = self.devices.keys().copied().collect();
        for handle in handles {
            match self.tell(owner, handle) {
                Sent::Taken => {}
                Sent::Full => {
                    self.full = true;
                    return;
                }
                Sent::Ended => {
                    self.ended(owner);
                    return;
                }
            }
        }
        if self.devices.is_empty() {
            self.connected = false;
            owner.close();
        }
    }

    /// The owner hung up: what it held of every device is released by its
    /// own end of the connection, and the next report opens another.
    pub fn ended(&mut self, owner: &mut impl Owner) {
        if self.connected {
            self.connected = false;
            owner.close();
        }
    }

    /// One device's difference, record by record; each one taken moves what
    /// the owner was told.
    fn tell(&mut self, owner: &mut impl Owner, handle: u32) -> Sent {
        let device = self.devices.get_mut(&handle).expect("listed");
        let source = device.source;
        if device.gone {
            let sent = owner.send(&Record::Gone { source });
            if sent == Sent::Taken {
                self.counts.sent += 1;
                self.devices.remove(&handle);
            }
            return sent;
        }
        // A key that moved and moved back since it was told is a press and
        // its release the owner will never see.
        let unseen = device.touched.without(Usages::symmetric(device.told.keys, device.now.keys));
        let mut keys = Keys::default();
        let _ = keys.hold(device.told.keys);
        for t in keys.hold(device.now.keys) {
            let modifiers = if t.pressed { 0 } else { MOD_RELEASED };
            let sent = owner.send(&Record::Key { source, key: RawKeyEvent { keycode: t.usage, modifiers } });
            if sent != Sent::Taken {
                return sent;
            }
            self.counts.sent += 1;
            if t.pressed {
                device.told.keys.insert(t.usage);
            } else {
                device.told.keys.remove(t.usage);
            }
        }
        self.counts.coalesced += u64::from(unseen.count());
        device.touched = Usages::NONE;

        let moved = device.now.dx != 0 || device.now.dy != 0 || device.now.scroll != 0;
        let changed = device.told.buttons != device.now.buttons || device.told.at != device.now.at;
        if moved || changed {
            let clamp16 = |v: i32| v.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
            let (dx, dy) = (clamp16(device.now.dx), clamp16(device.now.dy));
            let scroll = device.now.scroll.clamp(i32::from(i8::MIN), i32::from(i8::MAX));
            let motion = match device.now.at {
                Some((x, y)) => Motion::Absolute { x, y },
                None => Motion::Relative { dx: dx as i16, dy: dy as i16 },
            };
            let report = Report { source, buttons: device.now.buttons, scroll: scroll as i8, motion };
            let sent = owner.send(&Record::Pointer(report));
            if sent != Sent::Taken {
                return sent;
            }
            self.counts.sent += 1;
            let unseen = device.touched_buttons & !(device.told.buttons ^ device.now.buttons);
            self.counts.coalesced += u64::from(unseen.count_ones());
            device.touched_buttons = 0;
            device.told.buttons = device.now.buttons;
            device.told.at = device.now.at;
            if device.now.at.is_none() {
                device.now.dx -= dx;
                device.now.dy -= dy;
            }
            device.now.scroll -= scroll;
        }
        Sent::Taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toyos_input::wire::Record;

    const SECOND: Nanos = REOPEN_NS;

    #[derive(Default)]
    struct Mock {
        connected: bool,
        room: Option<usize>,
        refuses: bool,
        opens: u32,
        connections: Vec<Vec<Record>>,
    }

    impl Owner for Mock {
        fn open(&mut self) -> Opened {
            self.opens += 1;
            if self.refuses {
                return Opened::Refused;
            }
            self.connected = true;
            self.connections.push(Vec::new());
            Opened::Opened
        }
        fn send(&mut self, record: &Record) -> Sent {
            if !self.connected {
                return Sent::Ended;
            }
            match &mut self.room {
                Some(0) => return Sent::Full,
                Some(room) => *room -= 1,
                None => {}
            }
            self.connections.last_mut().unwrap().push(*record);
            Sent::Taken
        }
        fn close(&mut self) {
            self.connected = false;
        }
    }

    fn held(usages: &[u8]) -> Usages {
        let mut u = Usages::NONE;
        for &usage in usages {
            u.insert(usage);
        }
        u
    }

    fn key(source: u16, keycode: u8, pressed: bool) -> Record {
        Record::Key { source, key: RawKeyEvent { keycode, modifiers: if pressed { 0 } else { MOD_RELEASED } } }
    }

    /// At a full connection nothing waits in usbd: a release that falls while
    /// it is full reaches the owner once it drains, and a press and release
    /// inside it are counted, never delivered.
    #[test]
    fn at_a_full_input_connection_no_record_waits_and_a_release_reaches_the_owner_once_it_drains() {
        let mut inputs = Inputs::default();
        let mut owner = Mock::default();
        let kb = inputs.bind().unwrap();
        inputs.keys(kb, held(&[0x04]));
        inputs.flush(&mut owner, 0);
        assert_eq!(owner.connections, [vec![key(0, 0x04, true)]]);
        owner.room = Some(0);
        inputs.keys(kb, held(&[]));
        inputs.keys(kb, held(&[0x05]));
        inputs.keys(kb, held(&[]));
        inputs.flush(&mut owner, 1);
        assert!(inputs.full());
        owner.room = None;
        inputs.flush(&mut owner, 2);
        assert!(!inputs.full());
        assert_eq!(owner.connections, [vec![key(0, 0x04, true), key(0, 0x04, false)]]);
        assert_eq!(inputs.counts.coalesced, 1, "the press and release of 0x05 inside the full span");
    }

    /// A controller with only a stick opens nothing; the first bind opens the
    /// connection; the last device's `gone` is sent and the connection dropped.
    #[test]
    fn the_connection_follows_the_input_devices() {
        let mut inputs = Inputs::default();
        let mut owner = Mock::default();
        inputs.flush(&mut owner, 0);
        assert_eq!(owner.opens, 0, "a controller with nothing bound opened a connection");
        let kb = inputs.bind().unwrap();
        let mouse = inputs.bind().unwrap();
        inputs.flush(&mut owner, 1);
        assert_eq!((owner.opens, owner.connected), (1, true));
        inputs.gone(kb);
        inputs.flush(&mut owner, 2);
        assert!(owner.connected, "dropped with a device still bound");
        inputs.gone(mouse);
        inputs.flush(&mut owner, 3);
        assert_eq!(owner.connections, [vec![Record::Gone { source: 0 }, Record::Gone { source: 1 }]]);
        assert!(!owner.connected, "kept after the last gone");
    }

    /// The owner drops the connection while a key is held: the next report
    /// opens another, which is told the held key. A second drop, and a
    /// report inside the second after the last open, opens nothing and is
    /// counted; the first report a second after it opens again.
    #[test]
    fn a_bound_device_reopens_its_connection_on_its_next_report() {
        let mut inputs = Inputs::default();
        let mut owner = Mock::default();
        let kb = inputs.bind().unwrap();
        inputs.keys(kb, held(&[0x04]));
        inputs.flush(&mut owner, 0);
        inputs.ended(&mut owner);
        inputs.flush(&mut owner, 2 * SECOND);
        assert_eq!(owner.opens, 1, "reopened with no report");
        inputs.keys(kb, held(&[0x04, 0x05]));
        inputs.flush(&mut owner, 2 * SECOND);
        assert_eq!(owner.opens, 2);
        assert_eq!(owner.connections[1], [key(0, 0x04, true), key(0, 0x05, true)]);
        inputs.ended(&mut owner);
        let unowned = inputs.counts.unowned;
        inputs.keys(kb, held(&[0x04]));
        inputs.flush(&mut owner, 2 * SECOND + SECOND / 2);
        assert_eq!(owner.opens, 2, "a report inside the second opened");
        assert_eq!(inputs.counts.unowned, unowned + 1);
        inputs.keys(kb, held(&[]));
        inputs.flush(&mut owner, 3 * SECOND);
        assert_eq!(owner.opens, 3);
        assert_eq!(owner.connections[2], [], "a fresh owner is told the state now: nothing held");
    }

    /// One connection carries at most `DEVICES` devices.
    #[test]
    fn a_device_past_what_a_connection_carries_is_not_bound() {
        let mut inputs = Inputs::default();
        for _ in 0..DEVICES {
            assert!(inputs.bind().is_some());
        }
        assert!(inputs.bind().is_none());
    }
}
