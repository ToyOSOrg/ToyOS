//! What the machine's AML asks of the machine, answered through the kernel:
//! [`Firmware`] is the interpreter's [`Host`] over the `acpi` claim's
//! mediated access ([`toyos_abi::acpi`]), and [`Kernel`] is that access as
//! this server asks for it, so a host test answers in the kernel's place.
//!
//! **Nothing is written.** Loading a machine's tables writes nothing, and
//! this server evaluates nothing that does yet: a write in any space is
//! denied by name, and so is every access to the embedded controller's
//! space, which no transaction serves for AML yet. SystemCMOS never arrives:
//! the interpreter refuses that space itself. A read of memory, of a port or
//! of a function's configuration space is the kernel's to make or refuse, and
//! a refusal it names is denied under that name.
//!
//! **The Global Lock is the kernel's to exchange** (ACPI 6.5 §5.2.10.1). A
//! take that finds the firmware holding it waits for the firmware's release
//! and takes again, for at most [`LOCK_WAIT`] in one evaluation; past it the
//! take is denied by name.
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

/// The longest one evaluation waits for the firmware to let the Global Lock
/// go, over all its takes: what the interpreter lets one evaluation ask to
/// sleep.
pub const LOCK_WAIT: Duration = Duration::from_secs(10);

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
    /// The firmware holds it, and was left the request to say when it lets go.
    Pending,
    /// The machine's FACS is one the kernel exchanges no lock word in.
    Unusable,
}

/// The `acpi` claim's mediated access, as this server asks for it.
pub trait Kernel {
    fn access(&self, access: Access) -> Result<Answer, Stopping>;
    fn lock_take(&self) -> Result<Take, Stopping>;
    fn lock_release(&self) -> Result<(), Stopping>;
    /// Wait, for at most `within`, for the firmware to say it let the Global
    /// Lock go: how long that took, or `None` where it did not.
    fn released(&self, within: Duration) -> Option<Duration>;
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
    /// What is left of [`LOCK_WAIT`] in this evaluation.
    lock_wait: Duration,
    /// Reads made, by [`Space`].
    pub reads: [u64; 3],
    pub pages: Pages,
    /// Takes of the Global Lock, and how many found the firmware holding it.
    pub takes: u64,
    pub contended: u64,
    pub notifies: u64,
    pub refused: Ledger,
    notified: Ledger,
    /// The kernel answered that the machine is stopping.
    pub stopping: bool,
}

impl<'k, K: Kernel> Firmware<'k, K> {
    pub fn new(kernel: &'k K) -> Self {
        Firmware {
            kernel,
            began: Instant::now(),
            lock_wait: LOCK_WAIT,
            reads: [0; 3],
            pages: Pages::default(),
            takes: 0,
            contended: 0,
            notifies: 0,
            refused: Ledger::default(),
            notified: Ledger::default(),
            stopping: false,
        }
    }

    /// A load or an evaluation begins: its waits are its own.
    pub fn begin(&mut self) {
        self.lock_wait = LOCK_WAIT;
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
            Address::Memory(_) => "a write to SystemMemory: this server writes nothing for AML yet".into(),
            Address::Io(_) => "a write to SystemIO: this server writes nothing for AML yet".into(),
            Address::PciConfig { .. } => "a write to PCI_Config: this server writes nothing for AML yet".into(),
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
        if self.notified.see(&format!("{object} {value:#x}")) {
            println!("{OWN}Notify({object}, {value:#x}) for the first time; nothing serves a Notify yet");
        }
    }

    fn global_lock(&mut self, take: bool) -> Result<(), Denied> {
        if !take {
            return self.kernel.lock_release().map_err(|Stopping| self.stopped());
        }
        self.takes += 1;
        let mut found_held = false;
        loop {
            match self.kernel.lock_take() {
                Err(Stopping) => return Err(self.stopped()),
                Ok(Take::Taken) => return Ok(()),
                Ok(Take::Unusable) => {
                    return Err(self.deny("the Global Lock: the kernel exchanges no lock word in this machine's FACS".into(), format_args!("a take")))
                }
                Ok(Take::Pending) => {}
            }
            if !found_held {
                found_held = true;
                self.contended += 1;
            }
            let waited = if self.lock_wait.is_zero() { None } else { self.kernel.released(self.lock_wait) };
            let Some(waited) = waited else {
                self.lock_wait = Duration::ZERO;
                return Err(self.deny(
                    format!("the Global Lock: the firmware kept it for the {LOCK_WAIT:?} one evaluation waits"),
                    format_args!("a take the firmware was left the request for"),
                ));
            };
            self.lock_wait = self.lock_wait.saturating_sub(waited);
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
        /// What each wait for the firmware answers, in turn; `None` after.
        pub releases: RefCell<VecDeque<Option<Duration>>>,
        pub waits: RefCell<Vec<Duration>>,
        pub held: Cell<bool>,
        /// Accesses and lock exchanges answered before the machine stops.
        pub stops_after: Cell<Option<usize>>,
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
            assert_eq!(access.write, 0, "this server asks the kernel for no write");
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

        fn released(&self, within: Duration) -> Option<Duration> {
            self.waits.borrow_mut().push(within);
            self.releases.borrow_mut().pop_front().flatten()
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

    #[test]
    fn nothing_is_written_and_the_controllers_space_is_not_reached() {
        let kernel = machine();
        let mut host = Firmware::new(&kernel);
        let function = Address::PciConfig { segment: 0, bus: 0, device: 0x1F, function: 3, offset: 0x40 };
        for (at, space) in [(Address::Memory(NVS), "SystemMemory"), (Address::Io(0xB2), "SystemIO"), (function, "PCI_Config")] {
            let denied = host.write(at, toyos_aml::Access::Byte, 0).expect_err("a write was made");
            assert_eq!(denied.0, format!("a write to {space}: this server writes nothing for AML yet"));
        }
        let controller = Address::EmbeddedControl(0x38);
        assert_eq!(host.write(controller, toyos_aml::Access::Byte, 1), Err(Denied(format!("a write to {NO_CONTROLLER}"))));
        assert_eq!(host.read(controller, toyos_aml::Access::Byte), Err(Denied(format!("a read of {NO_CONTROLLER}"))));
        assert!(kernel.asked.borrow().is_empty(), "the kernel was asked for an access this server denies itself");
        assert_eq!(host.reads, [0, 0, 0]);
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
        assert!(kernel.waits.borrow().is_empty(), "a lock that was free was waited for");
    }

    #[test]
    fn a_lock_the_firmware_holds_is_waited_for_and_taken_again() {
        let kernel = machine();
        kernel.takes.borrow_mut().extend([Take::Pending, Take::Pending, Take::Taken]);
        kernel.releases.borrow_mut().extend([Some(Duration::from_secs(4)), Some(Duration::from_secs(1))]);
        let mut host = Firmware::new(&kernel);
        assert_eq!(host.global_lock(true), Ok(()));
        assert!(kernel.held.get());
        assert_eq!((host.takes, host.contended), (1, 1), "one take that found the firmware holding it, however often it asked");
        assert_eq!(*kernel.waits.borrow(), [LOCK_WAIT, LOCK_WAIT - Duration::from_secs(4)]);
        assert_eq!(host.global_lock(false), Ok(()));

        // What that take waited is spent for every later take of the evaluation.
        kernel.takes.borrow_mut().extend([Take::Pending, Take::Taken]);
        kernel.releases.borrow_mut().push_back(Some(Duration::from_secs(5)));
        assert_eq!(host.global_lock(true), Ok(()));
        assert_eq!(kernel.waits.borrow().last(), Some(&Duration::from_secs(5)));
        assert_eq!(host.global_lock(false), Ok(()));
    }

    #[test]
    fn a_lock_the_firmware_keeps_past_the_wait_is_denied_by_name() {
        let kept = format!("the Global Lock: the firmware kept it for the {LOCK_WAIT:?} one evaluation waits");

        // The firmware never says it let go.
        let kernel = machine();
        kernel.takes.borrow_mut().extend([Take::Pending; 4]);
        let mut host = Firmware::new(&kernel);
        assert_eq!(host.global_lock(true), Err(Denied(kept.clone())));
        assert_eq!(*kernel.waits.borrow(), [LOCK_WAIT]);
        assert!(!kernel.held.get());
        // Nothing is left of this evaluation's wait: the next take asks once and is denied unwaited.
        assert_eq!(host.global_lock(true), Err(Denied(kept.clone())));
        assert_eq!(kernel.waits.borrow().len(), 1);
        // The next evaluation waits again.
        host.begin();
        assert_eq!(host.global_lock(true), Err(Denied(kept.clone())));
        assert_eq!(*kernel.waits.borrow(), [LOCK_WAIT, LOCK_WAIT]);
        assert_eq!(host.refused.counts(), format!("{kept} x3"));

        // The firmware says it let go, and holds it again at every take: the
        // waits it answers add up to the bound, and no take follows it.
        let kernel = machine();
        kernel.takes.borrow_mut().extend([Take::Pending; 8]);
        kernel.releases.borrow_mut().extend([Some(Duration::from_secs(6)), Some(Duration::from_secs(6)), Some(Duration::from_secs(6))]);
        let mut host = Firmware::new(&kernel);
        assert_eq!(host.global_lock(true), Err(Denied(kept)));
        assert_eq!(*kernel.waits.borrow(), [LOCK_WAIT, LOCK_WAIT - Duration::from_secs(6)]);
        assert_eq!(kernel.takes.borrow().len(), 5, "three takes were asked: one before each wait, and one after the last");
        assert_eq!((host.takes, host.contended), (1, 1));
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
        assert!(kernel.waits.borrow().is_empty());
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
