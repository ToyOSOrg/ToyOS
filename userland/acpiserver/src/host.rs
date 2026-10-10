//! What the machine's AML asks of the machine, answered through the kernel:
//! [`Firmware`] is the interpreter's [`Host`] over the `acpi` claim's
//! mediated access ([`toyos_abi::acpi`]), and [`Kernel`] is that access as
//! this server asks for it, so a host test answers in the kernel's place.
//!
//! **A port is the only thing written.** A read of memory, of a port or of a
//! function's configuration space, and a write to a port, is the kernel's to
//! make or refuse, and a refusal it names is denied under that name: the
//! kernel keeps every port it drives, and makes a byte for `SMI_CMD` a call
//! into the firmware or refuses it. A write to memory or to configuration
//! space is denied by name, and so is every access to the embedded
//! controller's space, which no transaction serves for AML yet. SystemCMOS
//! never arrives: the interpreter refuses that space itself.
//!
//! **The Global Lock is the kernel's to exchange** (ACPI 6.5 §5.2.10.1). A
//! take that finds the firmware holding it is denied by name ([`HELD`]) and
//! counted: this server waits for no release of the firmware's yet.
//!
//! **A Notify of 0x80 to a control-method power button is a press** (ACPI
//! 6.5 §4.8.2.2.1.2), counted in [`Firmware::presses`] for the server to
//! serve, once [`Firmware::buttons`] names the button; every other Notify is
//! said the first time and served by nothing.
//!
//! **The power-off's sleep type is the one thing handed to the kernel**
//! ([`Kernel::s5`]): the kernel writes the register, and reads no AML.
//!
//! **What a line says.** A refusal is said the first time it is seen and
//! counted after ([`Ledger`]), as its address space, the kernel's name for
//! the refusal and the memory type: a line that can be quoted anywhere. Its
//! address, its function and any name the firmware chose are the machine's
//! own and go on a line of their own, under [`OWN`].

use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, Instant};

use toyos_abi::acpi::{pci_address, Access, Refused, Space, Width, UNLISTED};
use toyos_aml::{Address, Denied, Host};

use crate::ledger::Ledger;

/// What opens a line that carries an address, a PCI function or a name the
/// firmware chose: this machine's own, and quoted in no record.
pub const OWN: &str = "acpiserver: (this machine's own, quoted in no record) ";

/// The denial of a take that found the firmware holding the Global Lock.
pub const HELD: &str = "the Global Lock: the firmware holds it, and this server waits for no release yet";

/// The kernel does nothing more for this claim: the machine is stopping.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stopping;

/// What the kernel answered one access: the value read, or its refusal, and
/// the UEFI type firmware's map gives a memory address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Answer {
    pub made: Result<u64, Refused>,
    pub memory_type: u8,
}

/// What the kernel answered a take of the Global Lock.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Take {
    Taken,
    /// The firmware holds it, and was left the request.
    Pending,
    /// The machine's FACS is one the kernel exchanges no lock word in.
    Unusable,
}

/// The `acpi` claim's mediated access, as this server asks for it.
pub trait Kernel {
    fn access(&self, access: Access) -> Result<Answer, Stopping>;
    fn lock_take(&self) -> Result<Take, Stopping>;
    fn lock_release(&self) -> Result<(), Stopping>;
    /// Hand the kernel `\_S5`'s `SLP_TYPa` for its power-off, which it takes
    /// once under a claim: `false` where it is wider than the register's
    /// field, and the kernel kept nothing.
    fn s5(&self, slp_typ_a: u64) -> Result<bool, Stopping>;
}

/// Why a read was not made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    Kernel { space: Space, refused: Refused, memory_type: u8 },
    Stopping,
}

fn space_name(space: Space) -> &'static str {
    match space {
        Space::SystemMemory => "SystemMemory",
        Space::SystemIo => "SystemIO",
        Space::PciConfig => "PCI_Config",
    }
}

/// What memory of a UEFI type is called: an address firmware's map does not
/// list has no type, and a name of its own.
fn type_name(memory_type: u8) -> String {
    match memory_type {
        UNLISTED => "unlisted firmware memory".into(),
        listed => format!("memory of type {listed}"),
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kernel { space: Space::SystemMemory, refused, memory_type } => {
                write!(f, "a SystemMemory read the kernel refused {refused:?}, in {}", type_name(*memory_type))
            }
            Self::Kernel { space, refused, .. } => write!(f, "a {} read the kernel refused {refused:?}", space_name(*space)),
            Self::Stopping => f.write_str("the machine is stopping, and the kernel reads nothing more for this server"),
        }
    }
}

/// One read through the kernel: its value and, for memory, the type of what
/// was read.
pub fn read(kernel: &impl Kernel, space: Space, address: u64, width: Width) -> Result<(u64, u8), Refusal> {
    let Answer { made, memory_type } = kernel.access(Access::read(space, address, width)).map_err(|Stopping| Refusal::Stopping)?;
    match made {
        Ok(value) => Ok((value, memory_type)),
        Err(refused) => Err(Refusal::Kernel { space, refused, memory_type }),
    }
}

/// The distinct 4 KiB pages of memory read, each with the UEFI type
/// firmware's map gives it.
#[derive(Default)]
pub struct Pages(BTreeMap<u64, u8>);

impl Pages {
    pub fn read(&mut self, address: u64, memory_type: u8) {
        self.0.insert(address >> 12, memory_type);
    }

    /// How many pages of each type, and nothing of where they are.
    pub fn by_type(&self) -> String {
        let mut types: BTreeMap<u8, u64> = BTreeMap::new();
        for &memory_type in self.0.values() {
            *types.entry(memory_type).or_insert(0) += 1;
        }
        if types.is_empty() {
            return "none".into();
        }
        types.iter().map(|(&ty, pages)| format!("{pages} of {}", type_name(ty).trim_start_matches("memory of "))).collect::<Vec<_>>().join(", ")
    }
}

/// The interpreter's host on one machine, and what it was asked.
pub struct Firmware<'k, K> {
    kernel: &'k K,
    /// The zero of [`Host::timer`].
    began: Instant,
    /// Reads made, by [`Space`].
    pub reads: [u64; 3],
    pub pages: Pages,
    /// Takes of the Global Lock, and how many found the firmware holding it.
    pub takes: u64,
    pub contended: u64,
    pub notifies: u64,
    pub refused: Ledger,
    notified: Ledger,
    /// The paths of the control-method power buttons this server serves.
    pub buttons: Vec<String>,
    /// Presses of one of them notified and not yet served.
    pub presses: u64,
    /// The kernel answered that the machine is stopping.
    pub stopping: bool,
}

impl<'k, K: Kernel> Firmware<'k, K> {
    pub fn new(kernel: &'k K) -> Self {
        Firmware {
            kernel,
            began: Instant::now(),
            reads: [0; 3],
            pages: Pages::default(),
            takes: 0,
            contended: 0,
            notifies: 0,
            refused: Ledger::default(),
            notified: Ledger::default(),
            buttons: Vec::new(),
            presses: 0,
            stopping: false,
        }
    }

    /// Deny by the name `what`, said the first time with `own`, the machine's
    /// own detail of it.
    fn deny(&mut self, what: String, own: fmt::Arguments) -> Denied {
        if self.refused.see(&what) {
            println!("acpiserver: refused for the first time: {what}");
            println!("{OWN}that was {own}");
        }
        Denied(what)
    }

    fn stopped(&mut self) -> Denied {
        self.stopping = true;
        Denied(Refusal::Stopping.to_string())
    }
}

fn wide(width: toyos_aml::Access) -> Width {
    match width {
        toyos_aml::Access::Byte => Width::Byte,
        toyos_aml::Access::Word => Width::Word,
        toyos_aml::Access::DWord => Width::DWord,
        toyos_aml::Access::QWord => Width::QWord,
    }
}

const NO_CONTROLLER: &str = "EmbeddedControl: this server runs no controller transaction for AML yet";

impl<K: Kernel> Host for Firmware<'_, K> {
    fn read(&mut self, at: Address, width: toyos_aml::Access) -> Result<u64, Denied> {
        let (space, address) = match at {
            Address::Memory(address) => (Space::SystemMemory, address),
            Address::Io(port) => (Space::SystemIo, u64::from(port)),
            Address::PciConfig { segment, bus, device, function, offset } => (Space::PciConfig, pci_address(segment, bus, device, function, offset)),
            Address::EmbeddedControl(_) => return Err(self.deny(format!("a read of {NO_CONTROLLER}"), format_args!("{at:x?}"))),
        };
        match read(self.kernel, space, address, wide(width)) {
            Ok((value, memory_type)) => {
                self.reads[space as usize] += 1;
                if space == Space::SystemMemory {
                    self.pages.read(address, memory_type);
                }
                Ok(value)
            }
            Err(Refusal::Stopping) => Err(self.stopped()),
            Err(refusal) => Err(self.deny(refusal.to_string(), format_args!("{width:?} at {at:x?}"))),
        }
    }

    fn write(&mut self, at: Address, width: toyos_aml::Access, value: u64) -> Result<(), Denied> {
        let what = match at {
            Address::Io(port) => {
                return match self.kernel.access(Access::write(Space::SystemIo, u64::from(port), wide(width), value)) {
                    Err(Stopping) => Err(self.stopped()),
                    Ok(Answer { made: Ok(_), .. }) => Ok(()),
                    Ok(Answer { made: Err(refused), .. }) => Err(self.deny(
                        format!("a SystemIO write the kernel refused {refused:?}"),
                        format_args!("{width:?} {value:#x} to {at:x?}"),
                    )),
                };
            }
            Address::Memory(_) => "a write to SystemMemory: this server writes no memory for AML".into(),
            Address::PciConfig { .. } => "a write to PCI_Config: this server writes no configuration space for AML".into(),
            Address::EmbeddedControl(_) => format!("a write to {NO_CONTROLLER}"),
        };
        Err(self.deny(what, format_args!("{width:?} {value:#x} to {at:x?}")))
    }

    fn sleep(&mut self, ms: u64) {
        // The firmware's own delay (§19.6.125), bounded by the interpreter in
        // what one evaluation may ask for.
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn stall(&mut self, us: u64) {
        let until = Instant::now() + Duration::from_micros(us);
        while Instant::now() < until {
            std::hint::spin_loop();
        }
    }

    fn timer(&mut self) -> u64 {
        (self.began.elapsed().as_nanos() / 100) as u64
    }

    fn notify(&mut self, object: &str, value: u64) {
        self.notifies += 1;
        // §5.6.6's notification values of a power button: 0x80 is a press in S0.
        let press = value == 0x80 && self.buttons.iter().any(|button| button == object);
        if press {
            self.presses += 1;
        }
        if self.notified.see(&format!("{object} {value:#x}")) {
            let served = if press { "a press of the power button" } else { "served by nothing" };
            println!("{OWN}Notify({object}, {value:#x}) for the first time: {served}");
        }
    }

    fn global_lock(&mut self, take: bool) -> Result<(), Denied> {
        if !take {
            return self.kernel.lock_release().map_err(|Stopping| self.stopped());
        }
        self.takes += 1;
        match self.kernel.lock_take() {
            Err(Stopping) => Err(self.stopped()),
            Ok(Take::Taken) => Ok(()),
            Ok(Take::Unusable) => {
                Err(self.deny("the Global Lock: the kernel exchanges no lock word in this machine's FACS".into(), format_args!("a take")))
            }
            Ok(Take::Pending) => {
                self.contended += 1;
                Err(self.deny(HELD.into(), format_args!("a take the firmware was left the request for")))
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    use super::*;

    /// A kernel that answers reads from bytes at addresses, each range with a
    /// memory type, refuses `kept` ranges as the real one refuses memory of a
    /// type it passes no read of and every other address as it refuses RAM,
    /// and answers the Global Lock from a script.
    #[derive(Default)]
    pub struct Scripted {
        pub memory: Vec<(u64, u8, Vec<u8>)>,
        /// `(start, end, memory type)`.
        pub kept: Vec<(u64, u64, u8)>,
        pub ports: Vec<(u64, u64)>,
        pub config: Vec<(u64, u64)>,
        pub asked: RefCell<Vec<Access>>,
        /// What each take answers, in turn; `Taken` once it runs out.
        pub takes: RefCell<VecDeque<Take>>,
        pub held: Cell<bool>,
        /// Accesses and lock exchanges answered before the machine stops.
        pub stops_after: Cell<Option<usize>>,
        /// The `SLP_TYPa` it was handed and kept.
        pub handed: Cell<Option<u64>>,
    }

    impl Scripted {
        fn stopping(&self) -> Result<(), Stopping> {
            match self.stops_after.get() {
                Some(0) => Err(Stopping),
                Some(left) => {
                    self.stops_after.set(Some(left - 1));
                    Ok(())
                }
                None => Ok(()),
            }
        }
    }

    impl Kernel for Scripted {
        fn access(&self, access: Access) -> Result<Answer, Stopping> {
            self.stopping()?;
            self.asked.borrow_mut().push(access);
            assert!(access.write == 0 || access.space == Space::SystemIo as u8, "this server asks the kernel for no write but a port's");
            let width = Width::from_raw(access.width).expect("a width").bytes();
            let listed = |values: &[(u64, u64)]| values.iter().find(|(at, _)| *at == access.address).map(|&(_, value)| value);
            Ok(match Space::from_raw(access.space).expect("a space") {
                Space::SystemMemory => {
                    let held = self.memory.iter().find(|(base, _, bytes)| {
                        access.address >= *base && access.address + width <= base + bytes.len() as u64
                    });
                    match held {
                        Some((base, memory_type, bytes)) => {
                            let from = (access.address - base) as usize;
                            let value = bytes[from..from + width as usize].iter().rev().fold(0u64, |value, &byte| value << 8 | u64::from(byte));
                            Answer { made: Ok(value), memory_type: *memory_type }
                        }
                        None => match self.kept.iter().find(|(start, end, _)| (*start..*end).contains(&access.address)) {
                            Some(&(.., memory_type)) => Answer { made: Err(Refused::MemoryType), memory_type },
                            None => Answer { made: Err(Refused::UsableMemory), memory_type: 7 },
                        },
                    }
                }
                Space::SystemIo => Answer { made: listed(&self.ports).ok_or(Refused::KernelPort), memory_type: UNLISTED },
                Space::PciConfig => Answer { made: listed(&self.config).ok_or(Refused::ConfigUnreachable), memory_type: UNLISTED },
            })
        }

        fn lock_take(&self) -> Result<Take, Stopping> {
            self.stopping()?;
            let take = self.takes.borrow_mut().pop_front().unwrap_or(Take::Taken);
            if take == Take::Taken {
                assert!(!self.held.replace(true), "a lock already held was taken");
            }
            Ok(take)
        }

        fn lock_release(&self) -> Result<(), Stopping> {
            self.stopping()?;
            assert!(self.held.replace(false), "a lock nobody held was given back");
            Ok(())
        }

        fn s5(&self, slp_typ_a: u64) -> Result<bool, Stopping> {
            self.stopping()?;
            assert!(!self.held.get(), "the sleep type was handed over under the Global Lock");
            // ACPI 6.5 Table 4.16: `SLP_TYPx` is three bits.
            if slp_typ_a > 7 {
                return Ok(false);
            }
            assert_eq!(self.handed.replace(Some(slp_typ_a)), None, "a second sleep type under one claim, which the kernel refuses");
            Ok(true)
        }
    }

    const NVS: u64 = 0x7700_0000;

    fn machine() -> Scripted {
        Scripted {
            memory: vec![(NVS, 10, vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]), (NVS + 0x2000, 0, vec![0xAB; 4])],
            ports: vec![(0xB2, 0x5A)],
            config: vec![(pci_address(0, 0, 0x1F, 3, 0x40), 0x1234)],
            ..Default::default()
        }
    }

    #[test]
    fn a_read_is_the_kernels_in_each_space_and_counted_by_space_and_page() {
        let kernel = machine();
        let mut host = Firmware::new(&kernel);
        assert_eq!(host.read(Address::Memory(NVS), toyos_aml::Access::QWord), Ok(0x8877_6655_4433_2211));
        assert_eq!(host.read(Address::Memory(NVS + 2), toyos_aml::Access::Word), Ok(0x4433));
        assert_eq!(host.read(Address::Memory(NVS + 0x2000), toyos_aml::Access::Byte), Ok(0xAB));
        assert_eq!(host.read(Address::Io(0xB2), toyos_aml::Access::Byte), Ok(0x5A));
        let function = Address::PciConfig { segment: 0, bus: 0, device: 0x1F, function: 3, offset: 0x40 };
        assert_eq!(host.read(function, toyos_aml::Access::Word), Ok(0x1234));
        assert_eq!(
            *kernel.asked.borrow(),
            [
                Access::read(Space::SystemMemory, NVS, Width::QWord),
                Access::read(Space::SystemMemory, NVS + 2, Width::Word),
                Access::read(Space::SystemMemory, NVS + 0x2000, Width::Byte),
                Access::read(Space::SystemIo, 0xB2, Width::Byte),
                Access::read(Space::PciConfig, 0x00FB_0040, Width::Word),
            ]
        );
        assert_eq!(host.reads, [3, 1, 1]);
        assert_eq!(host.pages.by_type(), "1 of type 0, 1 of type 10", "two reads of one page are one page");
        assert!(host.refused.is_empty());
    }

    #[test]
    fn a_page_the_firmware_lists_nowhere_is_counted_under_a_name_of_its_own() {
        let mut pages = Pages::default();
        pages.read(0xfe00_0110, UNLISTED);
        pages.read(0xfe00_0ff0, UNLISTED);
        pages.read(NVS, 10);
        assert_eq!(pages.by_type(), "1 of type 10, 1 of unlisted firmware memory");
    }

    #[test]
    fn a_read_the_kernel_refuses_is_denied_under_the_kernels_name_and_counted() {
        let kernel = machine();
        let mut host = Firmware::new(&kernel);
        let ram = "a SystemMemory read the kernel refused UsableMemory, in memory of type 7";
        for _ in 0..3 {
            assert_eq!(host.read(Address::Memory(0x10_0000), toyos_aml::Access::DWord), Err(Denied(ram.into())));
        }
        // A read that runs off the end of what firmware holds is refused whole.
        assert_eq!(host.read(Address::Memory(NVS + 4), toyos_aml::Access::QWord), Err(Denied(ram.into())));
        let port = "a SystemIO read the kernel refused KernelPort";
        assert_eq!(host.read(Address::Io(0x70), toyos_aml::Access::Byte), Err(Denied(port.into())));
        let function = Address::PciConfig { segment: 1, bus: 0, device: 0, function: 0, offset: 0 };
        let config = "a PCI_Config read the kernel refused ConfigUnreachable";
        assert_eq!(host.read(function, toyos_aml::Access::DWord), Err(Denied(config.into())));
        assert_eq!(host.refused.counts(), format!("{config} x1; {port} x1; {ram} x4"));
        assert_eq!(host.reads, [0, 0, 0], "a refused read is no read made");
        assert_eq!(host.pages.by_type(), "none");
    }

    /// A port is written as the kernel answers, its refusal under its name;
    /// memory and configuration space are never written, and the
    /// controller's space is not reached, none of them asked of the kernel.
    #[test]
    fn only_a_port_is_written_and_the_controllers_space_is_not_reached() {
        let kernel = machine();
        let mut host = Firmware::new(&kernel);
        // An AMD laptop's `_Q28` writes its query's number to the POST port as a word.
        assert_eq!(host.write(Address::Io(0xB2), toyos_aml::Access::Word, 0x28), Ok(()));
        let refused = host.write(Address::Io(0x70), toyos_aml::Access::Byte, 1);
        assert_eq!(refused, Err(Denied("a SystemIO write the kernel refused KernelPort".into())));
        assert_eq!(
            *kernel.asked.borrow(),
            [
                Access::write(Space::SystemIo, 0xB2, Width::Word, 0x28),
                Access::write(Space::SystemIo, 0x70, Width::Byte, 1),
            ]
        );
        let function = Address::PciConfig { segment: 0, bus: 0, device: 0x1F, function: 3, offset: 0x40 };
        for (at, denied) in [
            (Address::Memory(NVS), "a write to SystemMemory: this server writes no memory for AML"),
            (function, "a write to PCI_Config: this server writes no configuration space for AML"),
        ] {
            assert_eq!(host.write(at, toyos_aml::Access::Byte, 0), Err(Denied(denied.into())));
        }
        let controller = Address::EmbeddedControl(0x38);
        assert_eq!(host.write(controller, toyos_aml::Access::Byte, 1), Err(Denied(format!("a write to {NO_CONTROLLER}"))));
        assert_eq!(host.read(controller, toyos_aml::Access::Byte), Err(Denied(format!("a read of {NO_CONTROLLER}"))));
        assert_eq!(kernel.asked.borrow().len(), 2, "the kernel was asked for an access this server denies itself");
        assert_eq!(host.reads, [0, 0, 0]);
    }

    /// A Notify of 0x80 to a button the server serves is a press, and to
    /// anything else, or of another value, is none.
    #[test]
    fn only_a_notify_of_a_press_to_a_served_button_is_a_press() {
        let kernel = machine();
        let mut host = Firmware::new(&kernel);
        host.notify("\\_SB_.PWRB", 0x80);
        assert_eq!(host.presses, 0, "a press of a button the server does not serve yet");
        host.buttons.push("\\_SB_.PWRB".into());
        for (object, value) in [("\\_SB_.PWRB", 0x02), ("\\_SB_.SLPB", 0x80), ("\\_SB_.PWRB_", 0x80), ("\\_SB_.LID_", 0x80)] {
            host.notify(object, value);
        }
        assert_eq!(host.presses, 0);
        host.notify("\\_SB_.PWRB", 0x80);
        host.notify("\\_SB_.PWRB", 0x80);
        assert_eq!((host.presses, host.notifies), (2, 7));
    }

    #[test]
    fn the_lock_is_taken_and_given_back_through_the_kernel() {
        let kernel = machine();
        let mut host = Firmware::new(&kernel);
        assert_eq!(host.global_lock(true), Ok(()));
        assert!(kernel.held.get());
        assert_eq!(host.global_lock(false), Ok(()));
        assert!(!kernel.held.get());
        assert_eq!((host.takes, host.contended), (1, 0));
        assert!(host.refused.is_empty());
    }

    #[test]
    fn a_lock_the_firmware_holds_is_denied_by_name_and_counted() {
        let kernel = machine();
        kernel.takes.borrow_mut().extend([Take::Pending, Take::Pending, Take::Taken]);
        let mut host = Firmware::new(&kernel);
        for _ in 0..2 {
            assert_eq!(host.global_lock(true), Err(Denied(HELD.into())));
            assert!(!kernel.held.get(), "a lock the firmware holds was reported taken");
        }
        assert_eq!(kernel.takes.borrow().len(), 1, "a denied take asked the kernel again");
        assert_eq!(host.refused.counts(), format!("{HELD} x2"));
        // The firmware let go: the next take is the kernel's answer again.
        assert_eq!(host.global_lock(true), Ok(()));
        assert_eq!(host.global_lock(false), Ok(()));
        assert_eq!((host.takes, host.contended), (3, 2));
    }

    #[test]
    fn a_lock_the_kernel_cannot_take_is_denied_and_never_reported_held() {
        let kernel = machine();
        kernel.takes.borrow_mut().push_back(Take::Unusable);
        let mut host = Firmware::new(&kernel);
        assert_eq!(
            host.global_lock(true),
            Err(Denied("the Global Lock: the kernel exchanges no lock word in this machine's FACS".into()))
        );
        assert_eq!(host.contended, 0);
    }

    #[test]
    fn a_stopping_machine_denies_everything_and_is_no_refusal_of_the_firmwares() {
        let kernel = machine();
        kernel.stops_after.set(Some(1));
        let mut host = Firmware::new(&kernel);
        assert_eq!(host.read(Address::Memory(NVS), toyos_aml::Access::Byte), Ok(0x11));
        assert!(!host.stopping);
        let stopping = Denied(Refusal::Stopping.to_string());
        assert_eq!(host.read(Address::Memory(NVS), toyos_aml::Access::Byte), Err(stopping.clone()));
        assert!(host.stopping);
        assert_eq!(host.global_lock(true), Err(stopping.clone()));
        assert_eq!(host.global_lock(false), Err(stopping));
        assert!(host.refused.is_empty(), "the stop was counted as something this machine's firmware was refused");
    }
}
