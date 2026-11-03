//! What the machine's AML asks of the machine, answered through the kernel:
//! [`Firmware`] is the interpreter's [`Host`] over the `acpi` claim's
//! mediated access ([`toyos_abi::acpi`]), and [`Kernel`] is that access as
//! this server asks for it, so a host test answers in the kernel's place.
//!
//! **A read of memory, of a port or of a function's configuration space is
//! the kernel's to make or refuse**, and a refusal it names is denied under
//! that name.
//!
//! **A port write is the kernel's to make or refuse, as a read is**, each
//! port and width said the first time it is asked, as the owner ruled on
//! #875. The kernel refuses the ports of the devices it drives
//! (`toyos_userbound::firmware::PROTECTED` among them), and makes a byte to the FADT's
//! `SMI_CMD` a call into the firmware, or refuses it by the byte and the rate.
//!
//! **A write to memory or configuration space is the kernel's to make or
//! refuse, as a read is**, and a refusal denies the evaluation, with one
//! exception: a register the kernel keeps for itself
//! ([`Refused::KernelRegister`]) is not written, said the first time per
//! register, and answered as written, so the method goes on. ACPI gives AML no
//! notion of a refused write, and the rest of the firmware's method, its own
//! record of what it set among it, is kept.
//!
//! **The first write the kernel makes into each page of memory firmware's map
//! lists nowhere or types memory-mapped I/O, and into each function's
//! configuration space, is said** with its address, width and value under
//! [`OWN`]: no fence of the kernel's knows what it is.
//!
//! **The spaces this server serves are SystemMemory, SystemIO, PCI_Config
//! and EmbeddedControl.** Every other space of Table 5.182 reaches it and is
//! denied by name: SystemCMOS, PciBarTarget, PCC, the OEM-defined ones, and
//! the command- and connection-based ones the interpreter carries whole
//! (SMBus, IPMI, GeneralPurposeIO, GenericSerialBus, PlatformRtMechanism,
//! FFixedHW), so `_REG` announces none of them.
//!
//! **A table the XSDT lists is found by its three identifiers**
//! ([`Host::find_table`]) through the kernel's reads, and answered only where
//! it lies in memory Table 15.6 calls AddressRangeReserved or
//! AddressRangeNVS, as `LoadTable` and a Data Table Region require.
//!
//! **The embedded controller's space is its own transactions'** ([`crate::ec`],
//! [`Firmware::transaction`]), a byte each, on the controller the server
//! attached ([`Firmware::ec`]) and through the kernel, which no row holds its
//! ports for, each under the Global Lock where the controller's `_GLK` asks
//! for it (ACPI 6.5 §6.5.7) and AML does not hold it already. A region is
//! available while its handler is (§6.5.4: "When an operation region
//! handler is unavailable, AML cannot access data fields in that region"),
//! and `_REG` only tells AML so: one attached is reached whether or not its
//! `_REG` has run, and with none attached the space is denied by name where
//! §6.5.4 has writes ignored and reads indeterminate. A port of it the kernel refuses
//! detaches the controller and is handed to the server ([`Firmware::ended`]),
//! which serves it no more.
//!
//! **The Global Lock's word is the kernel's to exchange** (ACPI 6.5
//! §5.2.10.1). A take that finds the firmware holding it leaves the firmware
//! the pending bit, waits for the release that bit asks for
//! ([`Kernel::released`]) and takes again, and is counted. A take the AML
//! bounds, an Acquire's, comes back untaken at its bound; one it does not
//! ends this server loudly where the firmware holds the lock past
//! [`RELEASE`].
//!
//! **A Notify of 0x80 to a control-method power button is a press** (ACPI
//! 6.5 §4.8.2.2.1.2), counted in [`Firmware::presses`] for the server to
//! serve, once [`Firmware::buttons`] names the button; every other Notify is
//! kept, once a value and an object, for the server to deliver after the
//! evaluation that made it ([`Firmware::notified`]).
//!
//! **An evaluation's Sleep serves the SCI to its deadline**
//! ([`Kernel::sleep`], `sci::await_sleep`): a press of the fixed button and
//! the controller's GPE are taken as they latch and kept
//! ([`Firmware::latched`]) for the server to serve after the evaluation, so a
//! press during a table's load is taken then, and powers the machine off once
//! the load has handed `\_S5` over.
//!
//! **A definition block AML loads at run time is read through the kernel
//! only from memory Table 15.6 calls AddressRangeReserved or
//! AddressRangeNVS** ([`Host::table`], ACPI 6.5 §19.6.76), each read's type
//! the kernel's answer; every load is said, its signature and length on a
//! line anyone may quote, its OEM Table ID and the method that loaded it
//! under [`OWN`].
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

use toyos_abi::acpi::{pci_address, Access, IdleDeclaration, Refused, Space, Width, UNLISTED};
use toyos_aml::{Address, Denied, Host, RuntimeLoad, Transfer};

use crate::devices::Ec;
use crate::ec::{Do, Transaction, Wait};
use crate::ledger::Ledger;
use crate::sci::Latched;

pub use acpiserver_api::OWN;
use acpiserver_api::{FIRST_WRITE_IN_PAGE, FIRST_WRITE_TO_FUNCTION, KEPT_WRITE};

/// The most the firmware may hold the Global Lock after a take the AML set no
/// bound on left it the pending bit: this server's bound, and no measurement,
/// over a lock SMM holds for one handler's run.
pub const RELEASE: Duration = Duration::from_secs(1);

/// The most a controller is waited for at one step of a transaction: one that
/// has not moved in this will not.
const EC_STEP: Duration = Duration::from_millis(500);

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

/// What the kernel did with a processor's idle states.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Declared {
    Kept,
    /// The processor's UID names no CPU the kernel runs.
    NoCpu,
    /// The CPU cannot enter them as declared; the kernel's log says why.
    Refused,
}

/// The `acpi` claim as this server asks it, its mediated access and its SCI,
/// and the power-off it asks of the supervisor.
pub trait Kernel {
    fn access(&self, access: Access) -> Result<Answer, Stopping>;
    fn lock_take(&self) -> Result<Take, Stopping>;
    fn lock_release(&self) -> Result<(), Stopping>;
    /// Waits until `until` for the firmware to give back a Global Lock a
    /// take left it the pending bit in ([`crate::sci::await_release`]):
    /// `false` is the deadline.
    fn released(&self, until: Instant) -> bool;
    /// Hand the kernel `\_S5`'s `SLP_TYPa` for its power-off, which it takes
    /// once under a claim.
    fn s5(&self, slp_typ_a: u64) -> Result<Handed, Stopping>;
    /// The machine is hardware-reduced (`toyos_abi::acpi::HARDWARE_REDUCED`):
    /// no fixed hardware, and a power-off that writes no sleep type.
    fn hardware_reduced(&self) -> bool;
    /// Declare the PCI root bridges, once ([`crate::apertures`]).
    fn declare(&self, roots: &[crate::apertures::Root]) -> Result<RootsDeclared, Stopping>;
    /// Hand the kernel one processor's idle states (`crate::cst`).
    fn idle(&self, declaration: &IdleDeclaration) -> Result<Declared, Stopping>;
    /// Waits until `until`, serving the SCI as `sci::await_sleep` does for
    /// the fixed power button where the claim serves it and the controller's
    /// GPE `ec_gpe`: what latched and was taken.
    fn sleep(&self, until: Instant, ec_gpe: Option<u16>) -> Latched;
    /// The SCIs the claim latched since its last acknowledgement, without
    /// waiting: how many, or `None` where none is.
    fn latched(&self) -> Option<u32>;
    /// Acknowledge the SCI, whose status this server has cleared.
    fn ack(&self);
    /// Ask the supervisor to power the machine off once its sessions have
    /// ended (`toyos::power::stop`, waiting): answered at once, the stop
    /// pending or refused.
    fn power_off(&self) -> Result<(), toyos::power::Refused>;
    /// Force the stop the last [`Self::power_off`] asked, and no other
    /// (`toyos::power::Pending::terminate`): refused `NoStopPending` once it
    /// has ended, whatever stop is pending since.
    fn terminate(&self) -> Result<(), toyos::power::Refused>;
}

/// What the kernel did with `\_S5`'s `SLP_TYPa`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Handed {
    Kept,
    /// Wider than the register's field: the kernel kept nothing.
    TooWide,
}

/// What the kernel did with a declaration of the root bridges' memory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RootsDeclared {
    Held,
    /// Refused whole, said in the kernel's log by name.
    Refused,
    /// The kernel already holds an answer: a declaration, or its refusal.
    Late,
}

/// Why an access to one of the controller's ports was not made.
pub enum Unmade {
    Stopping,
    /// The kernel keeps the port from this server, said as it refused it.
    Refused(String),
}

/// A byte of one of the controller's ports, which no row holds.
pub fn port_in(kernel: &impl Kernel, port: u16) -> Result<u8, Unmade> {
    let answer = kernel.access(Access::read(Space::SystemIo, u64::from(port), Width::Byte)).map_err(|Stopping| Unmade::Stopping)?;
    answer.made.map(|byte| byte as u8).map_err(|refused| Unmade::Refused(format!("the kernel refused a read of its port {port:#x}, {refused:?}")))
}

fn port_out(kernel: &impl Kernel, port: u16, byte: u8) -> Result<(), Unmade> {
    let answer = kernel.access(Access::write(Space::SystemIo, u64::from(port), Width::Byte, u64::from(byte))).map_err(|Stopping| Unmade::Stopping)?;
    answer.made.map(drop).map_err(|refused| Unmade::Refused(format!("the kernel refused a write of its port {port:#x}, {refused:?}")))
}

/// One transaction on the controller at `command` and `data`, waiting on it at
/// each step for at most [`EC_STEP`].
pub fn transact(kernel: &impl Kernel, command: u16, data: u16, mut tx: Transaction) -> Result<u8, Unmade> {
    let mut waiting: Option<(Wait, Instant)> = None;
    loop {
        let status = port_in(kernel, command)?;
        match tx.step(status) {
            Do::Wait(wait) => {
                let since = match waiting {
                    Some((was, since)) if was == wait => since,
                    _ => Instant::now(),
                };
                assert!(since.elapsed() < EC_STEP, "acpiserver: the embedded controller kept {wait:?} unmet for {EC_STEP:?} (EC_SC {status:#04x})");
                waiting = Some((wait, since));
                std::thread::yield_now();
            }
            Do::WriteCommand(byte) => {
                waiting = None;
                port_out(kernel, command, byte)?;
            }
            Do::WriteData(byte) => {
                waiting = None;
                port_out(kernel, data, byte)?;
            }
            Do::ReadData => {
                waiting = None;
                tx.read(port_in(kernel, data)?);
            }
            Do::Done(byte) => return Ok(byte),
        }
    }
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

/// Whether memory of a UEFI type is AddressRangeReserved or AddressRangeNVS
/// by Table 15.6: types 0, 5, 6, 8, 11, 12, 13 and 15 up, and 10. An address
/// firmware's map does not list is neither.
fn reserved_or_nvs(memory_type: u8) -> bool {
    memory_type != UNLISTED && matches!(memory_type, 0 | 5 | 6 | 8 | 10..=13 | 15..)
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
    /// Takes of the Global Lock, how many found the firmware holding it,
    /// and gives back.
    pub takes: u64,
    pub contended: u64,
    /// The longest a take waited for the firmware's release, or waited and
    /// gave up: the firmware's longest hold of the lock, at least, as far as a
    /// take saw it.
    pub longest_hold: Duration,
    pub given_back: u64,
    pub notifies: u64,
    /// The controller whose space AML reaches, once the server attached it.
    pub ec: Option<Ec>,
    /// Why a port of the attached controller was refused, which detached it,
    /// for the server to serve it no more.
    pub ended: Option<String>,
    pub refused: Ledger,
    /// The presses said, by button.
    said: Ledger,
    /// The port writes asked of the kernel, by port and width.
    written: Ledger,
    /// The writes the kernel made into a register's page or a function's
    /// configuration space, by page and by function.
    registers: Ledger,
    /// The writes the kernel kept from a register of its own, by register.
    pub kept: Ledger,
    /// The paths of the control-method power buttons this server serves.
    pub buttons: Vec<String>,
    /// What a Sleep took of the SCI, for the server to serve.
    pub latched: Latched,
    /// Each Notify but a press, once a value and an object, for the server
    /// to deliver.
    pub notified: std::collections::BTreeSet<(String, u64)>,
    /// Presses of one of them notified and not yet served.
    pub presses: u64,
    /// The kernel answered that the machine is stopping.
    pub stopping: bool,
    /// AML holds the Global Lock, which a transaction it makes runs under.
    locked: bool,
    /// Definition blocks loaded at run time, and refused.
    pub loaded: u64,
    pub loads_refused: u64,
    /// The RSDP the tables were found from, which `LoadTable` searches again.
    pub rsdp: u64,
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
            longest_hold: Duration::ZERO,
            given_back: 0,
            notifies: 0,
            ec: None,
            ended: None,
            refused: Ledger::default(),
            said: Ledger::default(),
            written: Ledger::default(),
            registers: Ledger::default(),
            kept: Ledger::default(),
            buttons: Vec::new(),
            latched: Latched::default(),
            notified: Default::default(),
            presses: 0,
            stopping: false,
            locked: false,
            loaded: 0,
            loads_refused: 0,
            rsdp: 0,
        }
    }

    /// One transaction on the controller at `command` and `data`, under the
    /// Global Lock where its `_GLK` asks for it (`glk`) and AML does not hold
    /// it already.
    pub fn transaction(&mut self, command: u16, data: u16, glk: bool, tx: Transaction) -> Result<u8, Unmade> {
        let lock = glk && !self.locked;
        if lock {
            match self.global_take() {
                Ok(()) => {}
                Err(_) if self.stopping => return Err(Unmade::Stopping),
                Err(Denied(why)) => return Err(Unmade::Refused(why)),
            }
        }
        let made = transact(self.kernel, command, data, tx);
        if lock {
            match self.global_release() {
                Ok(()) => {}
                Err(_) => return Err(Unmade::Stopping),
            }
        }
        made
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

    /// A write the kernel makes or refuses, a refusal denied under its name
    /// and, for memory, the type of what it was refused in; but a register
    /// the kernel keeps, which is answered as written.
    fn written_through(&mut self, space: Space, address: u64, width: toyos_aml::Access, value: u64, at: Address) -> Result<(), Denied> {
        match self.kernel.access(Access::write(space, address, wide(width), value)) {
            Err(Stopping) => Err(self.stopped()),
            Ok(Answer { made: Ok(_), memory_type }) => {
                let first = match space {
                    Space::SystemMemory if matches!(memory_type, UNLISTED | EFI_MMIO) => Some((format!("{FIRST_WRITE_IN_PAGE}{}", type_name(memory_type)), address >> 12)),
                    Space::PciConfig => Some((FIRST_WRITE_TO_FUNCTION.into(), address >> 16)),
                    _ => None,
                };
                if let Some((what, place)) = first {
                    if self.registers.see(&format!("{space:?} {place:#x}")) {
                        println!("{what}");
                        println!("{OWN}that was {width:?} {value:#x} to {at:x?}");
                    }
                }
                Ok(())
            }
            Ok(Answer { made: Err(Refused::KernelRegister), .. }) => {
                // Every register the kernel keeps is a qword or narrower, on its own qword.
                if self.kept.see(&format!("{:#x}", address & !7)) {
                    println!("{KEPT_WRITE}");
                    println!("{OWN}that was {width:?} {value:#x} to {at:x?}");
                }
                Ok(())
            }
            Ok(Answer { made: Err(refused), memory_type }) => {
                let what = match space {
                    Space::SystemMemory => format!("a SystemMemory write the kernel refused {refused:?}, in {}", type_name(memory_type)),
                    space => format!("a {} write the kernel refused {refused:?}", space_name(space)),
                };
                Err(self.deny(what, format_args!("{width:?} {value:#x} to {at:x?}")))
            }
        }
    }

    /// The denial of an access to a space this server serves no AML in.
    fn unserved(&mut self, at: Address, width: toyos_aml::Access) -> Denied {
        let space = match at {
            Address::Memory(_) => "SystemMemory",
            Address::Io(_) => "SystemIO",
            Address::PciConfig { .. } => "PCI_Config",
            Address::EmbeddedControl(_) => "EmbeddedControl",
            Address::SystemCmos(_) => "SystemCMOS",
            Address::PciBarTarget { .. } => "PciBarTarget",
            Address::Pcc { .. } => "PCC",
            Address::Oem { .. } => "an OEM-defined space",
        };
        self.deny(format!("an access to {space}, which this server serves no AML in"), format_args!("{width:?} at {at:x?}"))
    }

    fn stopped(&mut self) -> Denied {
        self.stopping = true;
        Denied(Refusal::Stopping.to_string())
    }

    /// One transaction on the attached controller for AML's `what` at `at`,
    /// or its denial by name where none is attached or the kernel refused
    /// one of its ports, which detaches it.
    fn controller(&mut self, what: &str, at: Address, width: toyos_aml::Access, tx: Transaction) -> Result<u8, Denied> {
        assert_eq!(width, toyos_aml::Access::Byte, "the interpreter reaches EmbeddedControl a byte at a time (Table 19.34)");
        let Some((command, data, glk)) = self.ec.as_ref().map(|ec| (ec.command, ec.data, ec.glk)) else {
            return Err(self.deny(format!("{what} {NO_CONTROLLER}"), format_args!("{at:x?}")));
        };
        match self.transaction(command, data, glk, tx) {
            Ok(byte) => Ok(byte),
            Err(Unmade::Stopping) => Err(self.stopped()),
            Err(Unmade::Refused(why)) => {
                self.ec = None;
                let denied = self.deny(format!("{what} EmbeddedControl: the kernel refused a port of the controller"), format_args!("{at:x?}: {why}"));
                self.ended = Some(why);
                Err(denied)
            }
        }
    }

    /// A take of the Global Lock, waiting for the firmware's release until
    /// `until` wherever it finds the firmware holding it: `false` is the
    /// deadline, and nothing taken.
    fn take(&mut self, until: Instant) -> Result<bool, Denied> {
        self.takes += 1;
        let mut contended = false;
        let asked = Instant::now();
        loop {
            match self.kernel.lock_take() {
                Err(Stopping) => return Err(self.stopped()),
                Ok(Take::Taken) => {
                    if contended {
                        self.longest_hold = self.longest_hold.max(asked.elapsed());
                    }
                    return Ok(true);
                }
                Ok(Take::Unusable) => {
                    let what = "the Global Lock: the kernel exchanges no lock word in this machine's FACS";
                    return Err(self.deny(what.into(), format_args!("a take")));
                }
                Ok(Take::Pending) => {}
            }
            if !contended {
                contended = true;
                self.contended += 1;
            }
            if !self.kernel.released(until) {
                self.longest_hold = self.longest_hold.max(asked.elapsed());
                return Ok(false);
            }
        }
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

const NO_CONTROLLER: &str = "EmbeddedControl: no controller is attached to this server's AML";

/// `EfiMemoryMappedIO`.
const EFI_MMIO: u8 = 11;

impl<K: Kernel> Host for Firmware<'_, K> {
    fn read(&mut self, at: Address, width: toyos_aml::Access) -> Result<u64, Denied> {
        let (space, address) = match at {
            Address::Memory(address) => (Space::SystemMemory, address),
            Address::Io(port) => (Space::SystemIo, u64::from(port)),
            Address::PciConfig { segment, bus, device, function, offset } => (Space::PciConfig, pci_address(segment, bus, device, function, offset)),
            Address::EmbeddedControl(address) => return self.controller("a read of", at, width, Transaction::read_at(address)).map(u64::from),
            other => return Err(self.unserved(other, width)),
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
        match at {
            Address::Io(port) => {
                if self.written.see(&format!("{port:#x} {width:?}")) {
                    println!("{OWN}a SystemIO write of {port:#x}, {width:?}, asked of the kernel for the first time");
                }
                self.written_through(Space::SystemIo, u64::from(port), width, value, at)
            }
            Address::Memory(address) => self.written_through(Space::SystemMemory, address, width, value, at),
            Address::PciConfig { segment, bus, device, function, offset } => {
                self.written_through(Space::PciConfig, pci_address(segment, bus, device, function, offset), width, value, at)
            }
            Address::EmbeddedControl(address) => {
                let byte = u8::try_from(value).expect("a byte access writes a byte");
                self.controller("a write to", at, width, Transaction::write_at(address, byte)).map(drop)
            }
            other => Err(self.unserved(other, width)),
        }
    }

    fn sleep(&mut self, ms: u64) {
        // The firmware's own delay (§19.6.125), bounded by the interpreter in
        // what one evaluation may ask for, the SCI served through it.
        let latched = self.kernel.sleep(Instant::now() + Duration::from_millis(ms), self.ec.as_ref().map(|ec| ec.gpe));
        self.latched.presses += latched.presses;
        self.latched.ec |= latched.ec;
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
        if value == 0x80 && self.buttons.iter().any(|button| button == object) {
            self.presses += 1;
            if self.said.see(object) {
                println!("{OWN}Notify({object}, {value:#x}) for the first time: a press of the power button");
            }
            return;
        }
        self.notified.insert((object.to_owned(), value));
    }

    fn global_take(&mut self) -> Result<(), Denied> {
        if self.take(Instant::now() + RELEASE)? {
            self.locked = true;
            return Ok(());
        }
        panic!("acpiserver: the firmware held the Global Lock {RELEASE:?} after a take left it the pending bit, and raised no GBL_STS");
    }

    fn global_take_within(&mut self, ms: u16) -> Result<bool, Denied> {
        let taken = self.take(Instant::now() + Duration::from_millis(ms.into()))?;
        self.locked = taken;
        Ok(taken)
    }

    fn global_release(&mut self) -> Result<(), Denied> {
        self.locked = false;
        self.kernel.lock_release().map_err(|Stopping| self.stopped())?;
        self.given_back += 1;
        Ok(())
    }

    fn table(&mut self, at: u64, len: usize) -> Result<Vec<u8>, Denied> {
        let mut bytes = Vec::with_capacity(len);
        while bytes.len() < len {
            let width = if len - bytes.len() >= 8 { Width::QWord } else { Width::Byte };
            let Some(address) = at.checked_add(bytes.len() as u64) else {
                return Err(self.deny("a definition block past the end of memory".into(), format_args!("{at:#x}+{len:#x}")));
            };
            match read(self.kernel, Space::SystemMemory, address, width) {
                Ok((value, memory_type)) if reserved_or_nvs(memory_type) => {
                    self.reads[Space::SystemMemory as usize] += 1;
                    self.pages.read(address, memory_type);
                    bytes.extend_from_slice(&value.to_le_bytes()[..width.bytes() as usize]);
                }
                Ok((_, memory_type)) => {
                    let what = format!("a definition block in {}, which Table 15.6 calls neither AddressRangeReserved nor AddressRangeNVS", type_name(memory_type));
                    return Err(self.deny(what, format_args!("{at:#x}+{len:#x}")));
                }
                Err(Refusal::Stopping) => return Err(self.stopped()),
                Err(refusal) => return Err(self.deny(refusal.to_string(), format_args!("a definition block at {at:#x}+{len:#x}"))),
            }
        }
        Ok(bytes)
    }

    fn transfer(&mut self, at: &Transfer<'_>, _: Option<&[u8]>) -> Result<Vec<u8>, Denied> {
        let space = match at.space {
            0x04 => "SMBus",
            0x07 => "IPMI",
            0x08 => "GeneralPurposeIO",
            0x09 => "GenericSerialBus",
            0x0B => "PlatformRtMechanism",
            _ => "FFixedHW",
        };
        Err(self.deny(format!("an access to {space}, which this server serves no AML in"), format_args!("{at:x?}")))
    }

    fn find_table(&mut self, signature: &[u8], oem_id: &[u8], oem_table_id: &[u8]) -> Result<Option<(u64, usize)>, Denied> {
        let tables = crate::tables::Tables::new(self.kernel);
        let found = toyos_acpi::find_by_id(&tables, self.rsdp, signature, oem_id, oem_table_id);
        if tables.stopping.get() {
            return Err(self.stopped());
        }
        let ids = format_args!("{:?} {:?} {:?}", signature.escape_ascii().to_string(), oem_id.escape_ascii().to_string(), oem_table_id.escape_ascii().to_string());
        let table = match found {
            Ok(Some(table)) => table,
            Ok(None) => return Ok(None),
            Err(why) => return Err(self.deny("a table LoadTable or a Data Table Region names: the XSDT was not read".into(), format_args!("{ids}: {why:x?}"))),
        };
        match read(self.kernel, Space::SystemMemory, table.base(), Width::Byte) {
            Ok((_, memory_type)) if reserved_or_nvs(memory_type) => Ok(Some((table.base(), table.len()))),
            Ok((_, memory_type)) => {
                let what = format!("a table LoadTable or a Data Table Region names, in {}, which Table 15.6 calls neither AddressRangeReserved nor AddressRangeNVS", type_name(memory_type));
                Err(self.deny(what, format_args!("{ids}")))
            }
            Err(Refusal::Stopping) => Err(self.stopped()),
            Err(refusal) => Err(self.deny(refusal.to_string(), format_args!("a table {ids}"))),
        }
    }

    fn loaded(&mut self, load: &RuntimeLoad<'_>) {
        let header = load.header.map(|header| {
            let text = |bytes: &[u8]| bytes.escape_ascii().to_string();
            (text(&header[..4]), u32::from_le_bytes(header[4..8].try_into().expect("four bytes")), text(&header[16..24]))
        });
        let said = header.as_ref().map_or("its header unread".into(), |(signature, length, _)| format!("{signature}, {length} bytes"));
        let oem = header.as_ref().map_or(String::new(), |(_, _, oem)| format!(", OEM Table ID {oem:?}"));
        match load.refused {
            None => {
                self.loaded += 1;
                println!("acpiserver: a table loaded at run time: {said}");
                println!("{OWN}{} loaded it{oem}", load.by);
            }
            Some(why) => {
                self.loads_refused += 1;
                println!("acpiserver: a table loaded at run time was refused: {said}: {}", crate::aml::kind(why));
                println!("{OWN}{} loaded it{oem}, refused {why:x?}", load.by);
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    use super::*;
    use crate::ec::tests::Emulated;

    /// The controller's ports as an AMD laptop's `_CRS` names them.
    pub const EC_DATA: u64 = 0x62;
    pub const EC_COMMAND: u64 = 0x66;

    /// A kernel that answers reads from bytes at addresses, each range with a
    /// memory type, refuses `typed_refused` ranges as the real one refuses memory of a
    /// type it passes no access to and every other address as it refuses RAM,
    /// writes memory where the real one does, which later reads see, but a
    /// register it `keeps`, writes a listed function's configuration space,
    /// and answers the Global Lock from a script.
    #[derive(Default)]
    pub struct Scripted {
        pub memory: Vec<(u64, u8, Vec<u8>)>,
        /// The bytes of `memory` a write changed, by address.
        pub written: RefCell<std::collections::BTreeMap<u64, u8>>,
        /// `(start, end, memory type)`.
        pub typed_refused: Vec<(u64, u64, u8)>,
        /// `(start, end)` of what the kernel drives, refused as device memory.
        pub driven: Vec<(u64, u64)>,
        /// `(start, end)` of registers the kernel keeps, whose write is
        /// refused `KernelRegister`.
        pub keeps: Vec<(u64, u64)>,
        pub ports: Vec<(u64, u64)>,
        pub config: Vec<(u64, u64)>,
        pub asked: RefCell<Vec<Access>>,
        /// What each take answers, in turn; `Taken` once it runs out.
        pub takes: RefCell<VecDeque<Take>>,
        pub held: Cell<bool>,
        /// What each wait for the firmware's release answers, in turn; the
        /// deadline once it runs out. Each deadline asked.
        pub releases: RefCell<VecDeque<bool>>,
        pub waited: RefCell<Vec<Instant>>,
        /// A controller at [`EC_DATA`] and [`EC_COMMAND`], answering its two
        /// ports before [`Self::ports`] is asked.
        pub controller: RefCell<Option<Emulated>>,
        /// The controller is reached only under the Global Lock, as a `_GLK`
        /// of 1 asks: a write to either port or a read of its data, which
        /// takes the byte, made without it panics.
        pub lock_rule: bool,
        /// Accesses and lock exchanges answered before the machine stops.
        pub stops_after: Cell<Option<usize>>,
        /// The `SLP_TYPa` it was handed and kept.
        pub handed: Cell<Option<u64>>,
        /// The machine is hardware-reduced.
        pub reduced: bool,
        /// The roots it was declared, once.
        pub roots: RefCell<Option<Vec<crate::apertures::Root>>>,
        /// Each processor's idle states it was handed, all kept.
        pub declared: RefCell<Vec<IdleDeclaration>>,
        /// What each Sleep's wait takes, in turn; nothing once it runs out.
        /// Each served set a Sleep was asked to serve.
        pub sleeps: RefCell<VecDeque<Latched>>,
        pub slept: RefCell<Vec<Option<u16>>>,
        /// What each power-off and terminate answers, in turn; each asked.
        pub stops: RefCell<VecDeque<Result<(), toyos::power::Refused>>>,
        pub stopped: RefCell<Vec<Stop>>,
    }

    /// What the server asked the supervisor.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Stop {
        PowerOff,
        Terminate,
    }

    /// What [`Scripted`] panics with where it is asked to power off with no
    /// answer scripted: the machine stops there, and nothing after the ask runs.
    pub const POWERED_OFF: &str = "the scripted machine powered off";

    impl Scripted {
        fn stop(&self, asked: Stop) -> Result<(), toyos::power::Refused> {
            self.stopped.borrow_mut().push(asked);
            self.stops.borrow_mut().pop_front().unwrap_or_else(|| panic!("{POWERED_OFF}"))
        }

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
            let width = Width::from_raw(access.width).expect("a width").bytes();
            let kept = self.keeps.iter().any(|&(start, end)| access.address < end && start < access.address + width);
            let listed = |values: &[(u64, u64)]| values.iter().find(|(at, _)| *at == access.address).map(|&(_, value)| value);
            Ok(match Space::from_raw(access.space).expect("a space") {
                Space::SystemMemory => {
                    let held = self.memory.iter().find(|(base, _, bytes)| {
                        access.address >= *base && access.address + width <= base + bytes.len() as u64
                    });
                    match held {
                        _ if access.write != 0 && kept => Answer { made: Err(Refused::KernelRegister), memory_type: UNLISTED },
                        // ACPI reclaim and runtime-services data, as the real one refuses them.
                        Some((_, memory_type @ (6 | 9), _)) if access.write != 0 => Answer { made: Err(Refused::TableWrite), memory_type: *memory_type },
                        Some((_, memory_type, _)) if access.write != 0 => {
                            let mut written = self.written.borrow_mut();
                            for i in 0..width {
                                written.insert(access.address + i, (access.value >> (8 * i)) as u8);
                            }
                            Answer { made: Ok(0), memory_type: *memory_type }
                        }
                        Some((base, memory_type, bytes)) => {
                            let written = self.written.borrow();
                            let value = (0..width).rev().fold(0u64, |value, i| {
                                let at = access.address + i;
                                value << 8 | u64::from(written.get(&at).copied().unwrap_or(bytes[(at - base) as usize]))
                            });
                            Answer { made: Ok(value), memory_type: *memory_type }
                        }
                        None if self.driven.iter().any(|(start, end)| (*start..*end).contains(&access.address)) => {
                            Answer { made: Err(Refused::DeviceMemory), memory_type: UNLISTED }
                        }
                        None => match self.typed_refused.iter().find(|(start, end, _)| (*start..*end).contains(&access.address)) {
                            Some(&(.., memory_type)) => Answer { made: Err(Refused::MemoryType), memory_type },
                            None => Answer { made: Err(Refused::UsableMemory), memory_type: 7 },
                        },
                    }
                }
                Space::SystemIo if self.controller.borrow().is_some() && [EC_DATA, EC_COMMAND].contains(&access.address) => {
                    if access.write != 0 || access.address == EC_DATA {
                        assert!(!self.lock_rule || self.held.get(), "a transaction on a controller whose _GLK asks for the Global Lock, made without it");
                    }
                    let mut controller = self.controller.borrow_mut();
                    let ec = controller.as_mut().expect("just seen");
                    let read = match (access.address, access.write != 0) {
                        (EC_COMMAND, false) => ec.status(),
                        (_, false) => ec.read(),
                        (EC_COMMAND, true) => {
                            ec.command(access.value as u8);
                            0
                        }
                        (_, true) => {
                            ec.data(access.value as u8);
                            0
                        }
                    };
                    Answer { made: Ok(u64::from(read)), memory_type: UNLISTED }
                }
                Space::SystemIo => Answer { made: listed(&self.ports).ok_or(Refused::KernelPort), memory_type: UNLISTED },
                Space::PciConfig if access.write != 0 => Answer { made: listed(&self.config).map(|_| 0).ok_or(Refused::ConfigUnreachable), memory_type: UNLISTED },
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

        fn released(&self, until: Instant) -> bool {
            assert!(!self.held.get(), "a release waited for under a lock already held");
            self.waited.borrow_mut().push(until);
            self.releases.borrow_mut().pop_front().unwrap_or(false)
        }

        fn sleep(&self, _: Instant, ec_gpe: Option<u16>) -> Latched {
            self.slept.borrow_mut().push(ec_gpe);
            self.sleeps.borrow_mut().pop_front().unwrap_or_default()
        }

        fn s5(&self, slp_typ_a: u64) -> Result<Handed, Stopping> {
            self.stopping()?;
            assert!(!self.held.get(), "the sleep type was handed over under the Global Lock");
            // ACPI 6.5 Table 4.16: `SLP_TYPx` is three bits.
            if slp_typ_a > 7 {
                return Ok(Handed::TooWide);
            }
            assert_eq!(self.handed.replace(Some(slp_typ_a)), None, "a second sleep type under one claim, which the kernel refuses");
            Ok(Handed::Kept)
        }

        fn hardware_reduced(&self) -> bool {
            self.reduced
        }

        fn declare(&self, roots: &[crate::apertures::Root]) -> Result<RootsDeclared, Stopping> {
            self.stopping()?;
            let mut declared = self.roots.borrow_mut();
            if declared.is_some() {
                return Ok(RootsDeclared::Late);
            }
            *declared = Some(roots.to_vec());
            Ok(if roots.is_empty() { RootsDeclared::Refused } else { RootsDeclared::Held })
        }

        fn idle(&self, declaration: &IdleDeclaration) -> Result<Declared, Stopping> {
            self.stopping()?;
            self.declared.borrow_mut().push(*declaration);
            Ok(Declared::Kept)
        }

        fn latched(&self) -> Option<u32> {
            None
        }

        fn ack(&self) {
            panic!("an SCI acknowledged where none latched");
        }

        fn power_off(&self) -> Result<(), toyos::power::Refused> {
            self.stop(Stop::PowerOff)
        }

        fn terminate(&self) -> Result<(), toyos::power::Refused> {
            self.stop(Stop::Terminate)
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

    /// Every port write is asked of the kernel and made or refused as it
    /// answers: the AMD bench's interrupt router's index and data, the POST
    /// port's word and a byte to `SMI_CMD`, made, and a port the kernel
    /// keeps, denied under its name for the refusal. A configuration write is
    /// asked as one; the controller's space with no controller attached is
    /// denied by name and asks the kernel nothing.
    #[test]
    fn every_port_write_is_asked_of_the_kernel_and_made_or_refused_as_it_answers() {
        use toyos_aml::Access::{Byte, Word};
        let mut kernel = machine();
        kernel.ports = vec![(0xC00, 0), (0xC01, 0), (0x80, 0), (0xB2, 0)];
        let mut host = Firmware::new(&kernel);
        let made = [(0xC00, Byte, 0x02), (0xC01, Byte, 0x0B), (0x80, Word, 0x28), (0xB2, Byte, 0xA0)];
        for (port, width, value) in made {
            assert_eq!(host.write(Address::Io(port), width, value), Ok(()), "{port:#x} {width:?}");
        }
        let kept = host.write(Address::Io(0xCF9), Byte, 0x06);
        assert_eq!(kept, Err(Denied("a SystemIO write the kernel refused KernelPort".into())));
        assert_eq!(
            *kernel.asked.borrow(),
            [
                Access::write(Space::SystemIo, 0xC00, Width::Byte, 0x02),
                Access::write(Space::SystemIo, 0xC01, Width::Byte, 0x0B),
                Access::write(Space::SystemIo, 0x80, Width::Word, 0x28),
                Access::write(Space::SystemIo, 0xB2, Width::Byte, 0xA0),
                Access::write(Space::SystemIo, 0xCF9, Width::Byte, 0x06),
            ]
        );
        let function = Address::PciConfig { segment: 0, bus: 0, device: 0x1F, function: 3, offset: 0x40 };
        assert_eq!(host.write(function, Byte, 0x5A), Ok(()));
        assert_eq!(kernel.asked.borrow()[5], Access::write(Space::PciConfig, 0x00FB_0040, Width::Byte, 0x5A));
        let controller = Address::EmbeddedControl(0x38);
        assert_eq!(host.write(controller, Byte, 1), Err(Denied(format!("a write to {NO_CONTROLLER}"))));
        assert_eq!(host.read(controller, Byte), Err(Denied(format!("a read of {NO_CONTROLLER}"))));
        assert_eq!(kernel.asked.borrow().len(), 6, "the kernel was asked for an access this server denies itself");
        assert_eq!(host.reads, [0, 0, 0]);
        assert_eq!(host.written.counts(), "0x80 Word x1; 0xb2 Byte x1; 0xc00 Byte x1; 0xc01 Byte x1; 0xcf9 Byte x1");
    }

    /// A write to memory is the kernel's to make or refuse: NVS and reserved
    /// memory are written, which a later read sees, and a write the kernel
    /// refuses, to the tables' memory or to runtime-services code, is denied
    /// under the kernel's name for it and the type, and counted.
    #[test]
    fn a_memory_write_is_the_kernels_to_make_or_refuse() {
        use toyos_aml::Access::{Byte, DWord, QWord};
        let mut kernel = machine();
        kernel.memory.push((0x7fb0_0000, 9, vec![0; 8]));
        kernel.typed_refused = vec![(0xfe00_0000, 0xfe00_1000, 5)];
        let mut host = Firmware::new(&kernel);
        assert_eq!(host.write(Address::Memory(NVS + 4), DWord, 0xdead_beef), Ok(()));
        assert_eq!(host.read(Address::Memory(NVS), QWord), Ok(0xdead_beef_4433_2211));
        assert_eq!(host.write(Address::Memory(NVS + 0x2000), Byte, 1), Ok(()));
        assert_eq!(host.read(Address::Memory(NVS + 0x2000), Byte), Ok(1));
        let tables = "a SystemMemory write the kernel refused TableWrite, in memory of type 9";
        assert_eq!(host.write(Address::Memory(0x7fb0_0000), Byte, 1), Err(Denied(tables.into())));
        let code = "a SystemMemory write the kernel refused MemoryType, in memory of type 5";
        for _ in 0..2 {
            assert_eq!(host.write(Address::Memory(0xfe00_0010), DWord, 0), Err(Denied(code.into())));
        }
        assert_eq!(host.refused.counts(), format!("{code} x2; {tables} x1"));
        assert!(host.registers.is_empty(), "a write to firmware memory or a refused one counted as a register's");
    }

    /// A write the kernel refuses `KernelRegister` is answered as made and
    /// denies nothing, said once per register, a register's being its
    /// aligned qword: the T14's two bytes of PL1 at 59A0h and 59A1h are one.
    /// Every other refusal still denies ([`a_memory_write_is_the_kernels_to_make_or_refuse`]).
    #[test]
    fn a_write_to_a_register_the_kernel_keeps_is_answered_as_made_and_said_once_a_register() {
        use toyos_aml::Access::{Byte, DWord};
        let mut kernel = machine();
        kernel.keeps = vec![(0xfedc_59a0, 0xfedc_59a8), (0xfedc_5418, 0xfedc_541c)];
        let mut host = Firmware::new(&kernel);
        for (at, width) in [(0xfedc_59a0, Byte), (0xfedc_59a1, Byte), (0xfedc_59a0, Byte), (0xfedc_59a4, DWord), (0xfedc_5418, DWord)] {
            assert_eq!(host.write(Address::Memory(at), width, 0x5A), Ok(()), "{at:#x}");
        }
        assert_eq!(kernel.asked.borrow().len(), 5, "each write was asked of the kernel");
        assert_eq!(host.kept.counts(), "0xfedc5418 x1; 0xfedc59a0 x4");
        assert!(host.refused.is_empty() && host.registers.is_empty());
    }

    /// The first write the kernel makes into each 4 KiB page firmware's map
    /// lists nowhere or types memory-mapped I/O, and into each function's
    /// configuration space, is said once: a later write there is counted, and
    /// a write to NVS or reserved memory is none of them.
    #[test]
    fn the_first_write_into_each_page_and_function_is_said_once() {
        use toyos_aml::Access::{Byte, DWord};
        let mut kernel = machine();
        kernel.memory.extend([(0xfd6e_0000, UNLISTED, vec![0; 0x2000]), (0xf800_0000, 11, vec![0; 0x100])]);
        kernel.config.push((pci_address(0, 0, 0x1F, 3, 0x44), 0));
        let mut host = Firmware::new(&kernel);
        let writes = [
            Address::Memory(0xfd6e_0a70),
            Address::Memory(0xfd6e_0a74),
            Address::Memory(0xfd6e_1000),
            Address::Memory(0xf800_00b8),
            Address::Memory(0xf800_00bc),
            Address::Memory(NVS),
            Address::PciConfig { segment: 0, bus: 0, device: 0x1F, function: 3, offset: 0x40 },
            Address::PciConfig { segment: 0, bus: 0, device: 0x1F, function: 3, offset: 0x44 },
        ];
        for at in writes {
            assert_eq!(host.write(at, Byte, 1), Ok(()), "{at:x?}");
        }
        assert_eq!(host.write(Address::Memory(0xf800_00b8), DWord, 0x3b1_0564), Ok(()));
        assert_eq!(host.registers.counts(), "PciConfig 0xfb x2; SystemMemory 0xf8000 x3; SystemMemory 0xfd6e0 x2; SystemMemory 0xfd6e1 x1");
    }

    /// Every space of Table 5.182 this server serves no AML in is denied by
    /// name and asks the kernel nothing: SystemCMOS, PciBarTarget, PCC and an
    /// OEM-defined space by address, and each carried whole.
    #[test]
    fn a_space_this_server_serves_no_aml_in_is_denied_by_name() {
        use toyos_aml::Access::Byte;
        let kernel = machine();
        let mut host = Firmware::new(&kernel);
        for (at, space) in [
            (Address::SystemCmos(0x40), "SystemCMOS"),
            (Address::PciBarTarget { segment: 0, bus: 0, device: 2, function: 0, bar: 0x10, offset: 0 }, "PciBarTarget"),
            (Address::Pcc { subspace: 2, offset: 4 }, "PCC"),
            (Address::Oem { space: 0x80, offset: 0 }, "an OEM-defined space"),
        ] {
            let denied = Err(Denied(format!("an access to {space}, which this server serves no AML in")));
            assert_eq!(host.read(at, Byte).map(drop), denied.clone());
            assert_eq!(host.write(at, Byte, 0), denied);
        }
        for (space, name) in [(0x04, "SMBus"), (0x07, "IPMI"), (0x08, "GeneralPurposeIO"), (0x09, "GenericSerialBus"), (0x0B, "PlatformRtMechanism"), (0x7F, "FFixedHW")] {
            let at = Transfer { space, region: 0, bit: 0, bits: 8, attrib: 0, length: 0, connection: None };
            assert_eq!(host.transfer(&at, None), Err(Denied(format!("an access to {name}, which this server serves no AML in"))));
        }
        assert!(kernel.asked.borrow().is_empty());
    }

    /// LoadTable's and a Data Table Region's table is found in the XSDT by
    /// its three identifiers and answered only in memory Table 15.6 calls
    /// reserved or NVS: QEMU's tables are ACPI reclaim memory, and denied.
    #[test]
    fn a_table_is_found_by_its_identifiers_only_in_reserved_or_nvs_memory() {
        use crate::aml::tests::{crafted_with, sealed, CRAFTED_RSDP};
        let mut oem = sealed(b"OEM1", &[0; 4]);
        oem[10..16].copy_from_slice(b"TOYOS ");
        oem[16..24].copy_from_slice(b"TABLE1  ");
        oem[9] = 0;
        oem[9] = 0u8.wrapping_sub(oem.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)));
        let mut kernel = crafted_with(&sealed(b"DSDT", &[]), &[], &[&oem]);
        let listed = kernel.memory.iter().position(|(_, _, bytes)| bytes.starts_with(b"OEM1")).expect("the table");
        let at = kernel.memory[listed].0;
        for (memory_type, found) in [(0, true), (10, true), (6, true), (9, false)] {
            kernel.memory[listed].1 = memory_type;
            let mut host = Firmware::new(&kernel);
            host.rsdp = CRAFTED_RSDP;
            let answer = host.find_table(b"OEM1", b"TOYOS", b"TABLE1");
            match found {
                true => assert_eq!(answer, Ok(Some((at, oem.len()))), "type {memory_type}"),
                false => assert_eq!(answer, Err(Denied("a table LoadTable or a Data Table Region names, in memory of type 9, which Table 15.6 calls neither AddressRangeReserved nor AddressRangeNVS".into()))),
            }
            assert_eq!(host.find_table(b"OEM1", b"TOYOS", b"TABLE2"), Ok(None));
        }
    }

    /// The controller's space is its own transactions', one a byte, through
    /// the kernel at the attached controller's ports: a read is RD_EC of its
    /// address, a write WR_EC of its address and value.
    #[test]
    fn the_controllers_space_is_read_and_written_through_its_transactions() {
        use toyos_aml::Access::Byte;
        let kernel = machine();
        let mut space = [0; 256];
        space[0xA8] = 0x5A;
        *kernel.controller.borrow_mut() = Some(Emulated::new(space));
        let mut host = Firmware::new(&kernel);
        host.ec = Some(Ec { path: "\\_SB_.EC0_".into(), command: EC_COMMAND as u16, data: EC_DATA as u16, gpe: 3, queries: Default::default(), glk: false });
        assert_eq!(host.read(Address::EmbeddedControl(0xA8), Byte), Ok(0x5A));
        assert_eq!(host.write(Address::EmbeddedControl(0x81), Byte, 0x01), Ok(()));
        assert_eq!(host.read(Address::EmbeddedControl(0x81), Byte), Ok(0x01));
        assert_eq!(kernel.controller.borrow().as_ref().expect("a controller").done, [(0x80, 0xA8, 0), (0x81, 0x81, 0x01), (0x80, 0x81, 0)]);
        let written: Vec<(u64, u64)> = kernel.asked.borrow().iter().filter(|access| access.write == 1).map(|access| (access.address, access.value)).collect();
        assert_eq!(written, [(EC_COMMAND, 0x80), (EC_DATA, 0xA8), (EC_COMMAND, 0x81), (EC_DATA, 0x81), (EC_DATA, 0x01), (EC_COMMAND, 0x80), (EC_DATA, 0x81)]);
        assert_eq!(host.reads, [0, 0, 0], "a byte of the controller's space counted as a port read of AML's");
        assert!(host.refused.is_empty() && host.ended.is_none());
    }

    /// A port of the attached controller the kernel refuses denies the access
    /// by name, detaches the controller and hands the server why; the next
    /// access is denied as one with none attached, and asks the kernel
    /// nothing. A stopping machine is no refusal and detaches nothing.
    #[test]
    fn a_controller_port_the_kernel_refuses_detaches_the_controller() {
        use toyos_aml::Access::Byte;
        let kernel = machine();
        let mut host = Firmware::new(&kernel);
        let ec = Ec { path: "\\_SB_.EC0_".into(), command: EC_COMMAND as u16, data: EC_DATA as u16, gpe: 3, queries: Default::default(), glk: false };
        host.ec = Some(ec.clone());
        let refused = host.read(Address::EmbeddedControl(0xA8), Byte);
        assert_eq!(refused, Err(Denied("a read of EmbeddedControl: the kernel refused a port of the controller".into())));
        assert_eq!(host.ec, None);
        assert_eq!(host.ended.as_deref(), Some("the kernel refused a read of its port 0x66, KernelPort"));
        let asked = kernel.asked.borrow().len();
        assert_eq!(host.write(Address::EmbeddedControl(0x81), Byte, 1), Err(Denied(format!("a write to {NO_CONTROLLER}"))));
        assert_eq!(kernel.asked.borrow().len(), asked, "a detached controller was asked again");

        let kernel = machine();
        kernel.stops_after.set(Some(0));
        let mut host = Firmware::new(&kernel);
        host.ec = Some(ec);
        assert_eq!(host.read(Address::EmbeddedControl(0xA8), Byte), Err(Denied(Refusal::Stopping.to_string())));
        assert!(host.stopping && host.ec.is_some() && host.ended.is_none());
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
        assert_eq!(host.global_take(), Ok(()));
        assert!(kernel.held.get());
        assert_eq!(host.global_release(), Ok(()));
        assert!(!kernel.held.get());
        assert_eq!((host.takes, host.contended, host.given_back), (1, 0, 1));
        assert!(kernel.waited.borrow().is_empty(), "a take the kernel answered taken waited");
        assert!(host.refused.is_empty());
    }

    /// §5.2.10.1: a take that finds the firmware holding the lock leaves it
    /// the pending bit and waits for the release that asks for; each release
    /// is followed by a take again, which may find the firmware holding it
    /// again.
    #[test]
    fn a_lock_the_firmware_holds_is_waited_for_and_taken_after_its_release() {
        let kernel = machine();
        kernel.takes.borrow_mut().extend([Take::Pending, Take::Pending, Take::Taken]);
        kernel.releases.borrow_mut().extend([true, true]);
        let mut host = Firmware::new(&kernel);
        let asked = Instant::now();
        assert_eq!(host.global_take(), Ok(()));
        assert!(kernel.held.get());
        assert!(kernel.takes.borrow().is_empty() && kernel.releases.borrow().is_empty());
        let waited = kernel.waited.borrow();
        assert_eq!(waited.len(), 2);
        assert!(waited.iter().all(|&until| until == waited[0] && until >= asked + RELEASE), "{waited:?}: not one deadline RELEASE after the take");
        assert_eq!(host.global_release(), Ok(()));
        assert_eq!((host.takes, host.contended, host.given_back), (1, 1, 1));
        assert!(host.refused.is_empty());
    }

    /// §19.6.2: an Acquire's TimeoutValue bounds the wait, however far past
    /// [`RELEASE`], and one that meets it comes back untaken, holding nothing.
    #[test]
    fn an_acquires_own_bound_ends_the_wait_untaken() {
        for ms in [5, 1000, 0xFFFE] {
            let kernel = machine();
            kernel.takes.borrow_mut().push_back(Take::Pending);
            let mut host = Firmware::new(&kernel);
            let asked = Instant::now();
            assert_eq!(host.global_take_within(ms), Ok(false));
            let returned = Instant::now();
            assert!(!kernel.held.get());
            let until = kernel.waited.borrow()[0];
            let bound = Duration::from_millis(ms.into());
            assert!(until >= asked + bound && until <= returned + bound, "{until:?} is not {bound:?} after the take at {asked:?}");
            assert_eq!((host.takes, host.contended, host.given_back), (1, 1, 0));
        }
    }

    #[test]
    #[should_panic(expected = "acpiserver: the firmware held the Global Lock 1s after a take left it the pending bit, and raised no GBL_STS")]
    fn a_firmware_that_holds_the_lock_past_the_bound_ends_the_server() {
        let kernel = machine();
        kernel.takes.borrow_mut().push_back(Take::Pending);
        let mut host = Firmware::new(&kernel);
        let _ = host.global_take();
    }

    #[test]
    fn a_lock_the_kernel_cannot_take_is_denied_and_never_reported_held() {
        let kernel = machine();
        kernel.takes.borrow_mut().push_back(Take::Unusable);
        let mut host = Firmware::new(&kernel);
        assert_eq!(
            host.global_take(),
            Err(Denied("the Global Lock: the kernel exchanges no lock word in this machine's FACS".into()))
        );
        assert_eq!(host.contended, 0);
        assert!(kernel.waited.borrow().is_empty());
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
        assert_eq!(host.global_take(), Err(stopping.clone()));
        assert_eq!(host.global_release(), Err(stopping));
        assert!(host.refused.is_empty(), "the stop was counted as something this machine's firmware was refused");
    }
}
