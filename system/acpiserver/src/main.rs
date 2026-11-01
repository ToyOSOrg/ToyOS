//! `/system/bin/acpiserver`: the machine's ACPI fixed hardware, served.
//!
//! The kernel puts the machine in ACPI mode when it mints this program's
//! `acpi` claim, and back in the mode its firmware handed over when the claim
//! goes with this process (`toyos_abi::acpi`). From then on every power
//! management event the firmware served in legacy mode is this server's, and
//! there is no restart: a fault ends it loudly, and the firmware has them
//! again.
//!
//! **At start** every fixed and general-purpose event is disabled and its
//! status cleared, then the fixed power button is enabled where the FADT
//! says the machine has one, and the SCI acknowledged: a press after the
//! clear latches and is served, one before it is lost. A hardware-reduced
//! machine (`toyos_abi::acpi::HARDWARE_REDUCED`) has no blocks and no SCI:
//! its tables are loaded and its namespace served, and no event.
//!
//! **Then the machine's tables are loaded** ([`aml::load`]), through the
//! kernel's mediated access ([`Claim`]): after the arming, so a press during
//! the load latches and is served when it ends. A table refused, and a DSDT
//! refused, are each said and survived; the power button is served either
//! way. The load hands the kernel `\_S5`'s sleep type, without which the
//! kernel refuses every power-off. A controller the ECDT names is attached to
//! AML before the first table loads.
//!
//! **Then what latched during the load is served** ([`Server::loaded`]), a
//! press taken by a table's Sleep and any SCI the load left latched: a press
//! then powers the machine off on the `\_S5` the load handed over, and waits
//! for no initialisation.
//!
//! **Then the namespace is initialised** ([`init`]): PCI_Config announced
//! where §6.5.4 asks it be, `\_SB._INI`, and the `_STA` and `_INI` of each
//! device at or above an `_INI` by Table 6.67, again over whatever a table
//! loaded meanwhile added.
//!
//! **Then the root bridges' memory is declared to the kernel**
//! ([`apertures`]), every PCI claim on the machine waiting for it.
//!
//! **Then the embedded controller is found in the namespace and armed**
//! ([`devices::find`], [`devices::attach`]): attached to the AML host in the ECDT's place, so its
//! space reaches it, under the Global Lock where its `_GLK` asks for it
//! (ACPI 6.5 §6.5.7), and announced by `_REG(3, 1)` in each scope that
//! declares a region of it (§6.5.4), after every `_INI`; then its GPE
//! enabled and its backlog drained. Its two ports are no row's, so every
//! access to them is the kernel's to make ([`Claim`]), and one it refuses
//! ends the controller, said by name, the server's own or AML's. A machine
//! whose DSDT did not load has no controller served.
//! **Where the FADT says the power button is a control method device**, the
//! buttons the namespace names are found too, and where one is, each query
//! is run as its method ([`run_query`]): a Notify of a press is a press.
//! Elsewhere no query's method runs.
//!
//! **Then `_OSC` is negotiated** ([`osc`]): `\_SB._OSC` query then commit,
//! and each processor's on an Intel processor committed, claiming what ToyOS
//! honours; whatever a table they loaded added is initialised in turn. Then
//! each processor's idle states are handed to the kernel ([`cst`]).
//!
//! **Each SCI** is read off both blocks ([`sci::events`]): a press stops the
//! machine through the supervisor, or is said and dropped on a machine with
//! no power-off, and the controller's GPE drains the
//! controller of every query waiting, which are then run, one by one, after
//! the drain that took them. An event this server
//! never enabled, a controller that does not answer, more queries in one
//! drain than [`QUERIES`], and [`sci::EMPTY_SCIS`] SCIs in a row that carried
//! nothing are each a panic naming the registers.
//!
//! Inside an evaluation, a take of the Global Lock the firmware holds waits
//! on the SCI for its release alone ([`sci::await_release`]), and what else
//! latched meanwhile is served after.
//!
//! **Then the machine's batteries and AC adapters are read** ([`battery`]),
//! once and every [`battery::POLL`] between SCIs: the controller's space is
//! reached there and in a query's method, never inside a drain.
//!
//! The log carries each query number the first time it is taken and a count
//! of every one at [`COUNTS`] intervals, never a line per event.

mod aml;
mod apertures;
mod battery;
mod cpu;
mod cst;
mod devices;
mod ec;
mod gpe;
mod host;
mod init;
mod ledger;
mod osc;
mod sci;
mod tables;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use toyos::endow;
use toyos::ioport::{in16, in8, out16, out8};
use toyos::poller::{Poller, READABLE};
use toyos::power::{self, Mode, Refused, Stop};
use toyos::AcpiDev;
use toyos_abi::acpi::{Access, AcpiInfo, Apertures, Block, IdleDeclaration, FIXED_POWER_BUTTON, HARDWARE_REDUCED};
use toyos_abi::syscall::{DeviceType, SyscallError};

use acpiserver_api::{CONTROLLER_NONE, CONTROLLER_SERVED};
use aml::Aml;
use battery::Power;
use devices::{Ec, Queried};
use ec::Transaction;
use host::{port_in, Answer, Declared, Handed, Kernel, RootsDeclared, Stopping, Take, Unmade, OWN};
use init::{Census, Names};
use sci::{Bits, Event, Fixed, Served, Unserved, Unstopped, PM1_STATUS, PWRBTN};

/// The most queries one drain takes before the controller is a storm.
const QUERIES: usize = 32;
/// How often the log is told the queries' counts, where they moved.
const COUNTS: Duration = Duration::from_secs(30);

struct Server<'a, K, P> {
    kernel: &'a K,
    ports: P,
    info: AcpiInfo,
    served: Served,
    /// The controller the namespace names, if it names one this server serves.
    ec: Option<Ec>,
    /// The namespace the load kept.
    aml: Option<Aml<'a, K>>,
    /// The batteries and adapters found in it, where one is a battery.
    power: Option<Power>,
    /// Taken off the controller by the drain, run after it.
    queued: VecDeque<u8>,
    /// Every query number taken, and how often.
    counts: BTreeMap<u8, u64>,
    counted: u64,
    logged: u64,
    scis: u64,
    empty: u32,
    /// The kernel was handed `\_S5`'s sleep type: a press powers the machine off.
    power_off: bool,
    /// A press asked a stop, which another press forces.
    stop_asked: bool,
    /// What the namespace's initialisation ran.
    census: Census,
    /// What `_OSC` answered.
    negotiated: osc::Negotiated,
    /// The processor's vendor and family.
    cpu: cpu::Cpu,
    /// The batteries and adapters the namespace names, which a Notify of an
    /// information change reads again.
    sources: (Vec<String>, Vec<String>),
    /// Each runtime GPE's methods run, by GPE.
    gpe_runs: BTreeMap<u16, u64>,
    /// Each Notify delivered, by object and value.
    delivered: ledger::Ledger,
    /// The processors whose `_CST` is read, which their Notify 0x81 reads again.
    idle: Vec<cst::Processor>,
}

fn main() {
    let Some(dev) = endow::device::<AcpiDev>(DeviceType::Acpi) else {
        println!("acpiserver: no acpi claim, so the machine stays in the mode its firmware handed over");
        return;
    };
    let info = dev.describe().expect("acpiserver: the claim's first read is its description");
    let claim = Claim { dev: &dev, info, poller: Poller::new(1), watching: Cell::new(false), stop: RefCell::new(None) };
    let mut internal = false;
    for arg in std::env::args().skip(1) {
        assert!(arg == apertures::PORTS_INTERNAL, "acpiserver: {arg:?} is no argument this server takes");
        internal = true;
    }
    let mut server = Server::new(&claim, Machine, info);
    if info.flags & HARDWARE_REDUCED == 0 {
        server.arm(&dev);
    }
    let (loaded, kept) = aml::load(&claim, server.info.rsdp);
    let names = server.loaded(loaded.handed, kept);
    server.declare(names.as_ref(), internal);
    let (batteries, adapters) = server.arm_controller(names.as_ref());
    if let Some(names) = server.negotiate(names) {
        println!("{}", server.census.line());
        server.declare_idle(&names);
        server.arm_gpes(&names);
    }
    server.sources = (batteries.clone(), adapters.clone());
    server.find_power(batteries, adapters);
    server.served_aml();
    server.serve();
}

/// The row's PM1 event and GPE0 registers, which this server reads and writes
/// itself.
trait Ports {
    fn in8(&self, port: u16) -> u8;
    fn out8(&self, port: u16, byte: u8);
    fn in16(&self, port: u16) -> u16;
    fn out16(&self, port: u16, word: u16);
}

/// The machine's own ports, which the claim opened to this process.
struct Machine;

impl Ports for Machine {
    fn in8(&self, port: u16) -> u8 {
        in8(port)
    }

    fn out8(&self, port: u16, byte: u8) {
        out8(port, byte)
    }

    fn in16(&self, port: u16) -> u16 {
        in16(port)
    }

    fn out16(&self, port: u16, word: u16) {
        out16(port, word)
    }
}

/// The claim as the tables' fetch and their AML ask it for what lies outside
/// its own ports, and as the wait for the firmware's release of the Global
/// Lock reads its event blocks and its SCI; and the one poller its SCI is
/// waited on with, between SCIs and inside that wait.
struct Claim<'a> {
    dev: &'a AcpiDev,
    info: AcpiInfo,
    poller: Poller,
    /// A watch answers once: one still registered from a wait that timed out
    /// is not registered again.
    watching: Cell<bool>,
    /// The stop the last press asked, which a terminate forces and no other.
    stop: RefCell<Option<power::Pending>>,
}

impl Claim<'_> {
    /// Waits until `until` for the claim's record to be readable.
    fn readable(&self, until: Instant) {
        if !self.watching.replace(true) {
            self.poller.watch(self.dev, READABLE, 0);
        }
        let wait = until.saturating_duration_since(Instant::now());
        self.poller.wait(1, wait.as_nanos() as u64, |_| self.watching.set(false));
    }

    /// The kernel's answer; a stopping machine's is the caller's to carry,
    /// and any other refusal of a call this server formed is this server's
    /// defect.
    fn answered<T>(asked: &str, answer: Result<T, SyscallError>) -> Result<T, Stopping> {
        match answer {
            Ok(answer) => Ok(answer),
            Err(SyscallError::Gone) => Err(Stopping),
            Err(other) => panic!("acpiserver: the kernel answered {asked} {other:?}"),
        }
    }
}

impl Kernel for Claim<'_> {
    fn access(&self, access: Access) -> Result<Answer, Stopping> {
        Self::answered("a mediated access", self.dev.access(access)).map(|(made, memory_type)| Answer { made, memory_type })
    }

    fn lock_take(&self) -> Result<Take, Stopping> {
        match self.dev.lock_take() {
            Err(SyscallError::NotSupported) => Ok(Take::Unusable),
            answer => Self::answered("a take of the Global Lock", answer).map(|taken| if taken { Take::Taken } else { Take::Pending }),
        }
    }

    fn lock_release(&self) -> Result<(), Stopping> {
        Self::answered("the Global Lock's release", self.dev.lock_release())
    }

    fn released(&self, until: Instant) -> bool {
        sci::await_release(&mut Waiting(self), until)
    }

    fn sleep(&self, until: Instant, ec_gpe: Option<u16>) -> sci::Latched {
        let served = Served { power_button: self.info.flags & FIXED_POWER_BUTTON != 0, ec_gpe, ..Default::default() };
        sci::await_sleep(&mut Waiting(self), until, &served)
    }

    fn s5(&self, slp_typ_a: u64) -> Result<Handed, Stopping> {
        match self.dev.s5(slp_typ_a) {
            Err(SyscallError::InvalidArgument) => Ok(Handed::TooWide),
            answer => Self::answered("the power-off's sleep type", answer).map(|()| Handed::Kept),
        }
    }

    fn hardware_reduced(&self) -> bool {
        self.info.flags & HARDWARE_REDUCED != 0
    }

    fn declare(&self, roots: &[apertures::Root]) -> Result<RootsDeclared, Stopping> {
        let mut declared = Apertures::default();
        for root in roots {
            declared.push(root.segment, root.buses, &root.windows, root.osc, root.committed).expect("acpiserver: the roots were read into a declaration's room");
        }
        match self.dev.declare(&declared) {
            Err(SyscallError::InvalidArgument) => Ok(RootsDeclared::Refused),
            Err(SyscallError::AlreadyExists) => Ok(RootsDeclared::Late),
            answer => Self::answered("the root bridges' memory", answer).map(|()| RootsDeclared::Held),
        }
    }

    fn idle(&self, declaration: &IdleDeclaration) -> Result<Declared, Stopping> {
        match self.dev.idle(declaration) {
            Err(SyscallError::NotFound) => Ok(Declared::NoCpu),
            Err(SyscallError::InvalidArgument) => Ok(Declared::Refused),
            answer => Self::answered("a processor's idle states", answer).map(|()| Declared::Kept),
        }
    }

    fn latched(&self) -> Option<u32> {
        match self.dev.irq() {
            Ok(record) => Some(record.count),
            Err(SyscallError::WouldBlock) => None,
            Err(other) => panic!("acpiserver: the claim's record answered {other:?}"),
        }
    }

    fn ack(&self) {
        self.dev.ack().expect("acpiserver: the claim's acknowledgement");
    }

    fn power_off(&self) -> Result<(), power::Refused> {
        let pending = power::stop(Stop::Shutdown, Mode::Wait)?;
        *self.stop.borrow_mut() = Some(pending);
        Ok(())
    }

    fn terminate(&self) -> Result<(), power::Refused> {
        self.stop.borrow().as_ref().map_or(Err(Refused::NoStopPending), power::Pending::terminate)
    }
}

/// The claim's event blocks and SCI for one wait for the firmware's release.
struct Waiting<'c, 'a>(&'c Claim<'a>);

impl Fixed for Waiting<'_, '_> {
    fn status(&mut self) -> Bits {
        Bits { pm1: in16(self.0.info.pm1_event.port), gpe0: bytes(self.0.info.gpe0).map(|(status, _)| in8(status)).collect() }
    }

    fn pm1_clear(&mut self, bits: u16) {
        out16(self.0.info.pm1_event.port, bits);
    }

    fn gpe_clear(&mut self, n: u16) {
        let (status, _) = bytes(self.0.info.gpe0).nth(usize::from(n / 8)).expect("a GPE this server serves is in the block");
        out8(status, 1 << (n % 8));
    }

    fn enables(&mut self) -> Bits {
        Bits { pm1: in16(self.0.info.pm1_event.enable()), gpe0: bytes(self.0.info.gpe0).map(|(_, enable)| in8(enable)).collect() }
    }

    fn enable(&mut self, enables: &Bits) {
        out16(self.0.info.pm1_event.enable(), enables.pm1);
        for ((_, enable), &byte) in bytes(self.0.info.gpe0).zip(&enables.gpe0) {
            out8(enable, byte);
        }
    }

    fn sci(&mut self, until: Instant) -> bool {
        if until.saturating_duration_since(Instant::now()).is_zero() {
            return false;
        }
        self.0.readable(until);
        match self.0.dev.irq() {
            Ok(_) | Err(SyscallError::WouldBlock) => true,
            Err(other) => panic!("acpiserver: the claim's record answered {other:?} while the Global Lock was waited for"),
        }
    }

    fn ack(&mut self) {
        Kernel::ack(self.0);
    }
}

/// Each byte of a status-and-enable block: its status port and its enable port.
fn bytes(block: Block) -> impl Iterator<Item = (u16, u16)> {
    (0..block.len / 2).map(move |i| (block.port + i, block.enable() + i))
}

impl<'a, K: Kernel, P: Ports> Server<'a, K, P> {
    fn new(kernel: &'a K, ports: P, info: AcpiInfo) -> Self {
        Server {
            kernel,
            ports,
            info,
            served: Served { power_button: info.flags & FIXED_POWER_BUTTON != 0, ..Default::default() },
            ec: None,
            aml: None,
            power: None,
            queued: VecDeque::new(),
            counts: BTreeMap::new(),
            counted: 0,
            logged: 0,
            scis: 0,
            empty: 0,
            power_off: false,
            stop_asked: false,
            census: Census::default(),
            negotiated: osc::Negotiated::default(),
            idle: Vec::new(),
            cpu: cpu::Cpu::here(),
            sources: (Vec::new(), Vec::new()),
            gpe_runs: BTreeMap::new(),
            delivered: ledger::Ledger::default(),
        }
    }

    fn arm(&mut self, dev: &AcpiDev) {
        let pm1 = self.info.pm1_event;
        self.ports.out16(pm1.enable(), 0);
        self.ports.out16(pm1.port, PM1_STATUS);
        for (status, enable) in bytes(self.info.gpe0) {
            self.ports.out8(enable, 0);
            self.ports.out8(status, 0xFF);
        }
        if self.served.power_button {
            self.ports.out16(pm1.enable(), PWRBTN);
        }
        dev.ack().expect("acpiserver: the claim's acknowledgement");
        println!(
            "acpiserver: armed: power button {}",
            if self.served.power_button { "served" } else { "a control method device, served once the namespace names it" },
        );
    }

    /// Take what the load kept: what latched during it is served first, so a
    /// press then powers the machine off on the `\_S5` it handed over and
    /// waits for no initialisation, and then the namespace is initialised
    /// ([`Server::initialise`]).
    fn loaded(&mut self, handed: bool, kept: Option<Aml<'a, K>>) -> Option<Names> {
        self.power_off = handed;
        self.aml = kept;
        self.served_aml();
        self.latched();
        self.initialise()
    }

    /// Each SCI already latched, served without waiting for one.
    fn latched(&mut self) {
        let Some(count) = self.kernel.latched() else { return };
        self.scis += u64::from(count);
        self.take();
        self.kernel.ack();
    }

    /// Declare the root bridges' memory ([`apertures`]) out of the namespace
    /// as initialised; with no namespace, no window.
    fn declare(&mut self, names: Option<&Names>, internal: bool) {
        let read = match (self.aml.as_mut(), names) {
            (Some(aml), Some(names)) => apertures::read(aml, names, &mut self.census, internal),
            _ => Err("no namespace was kept".into()),
        };
        if apertures::declare(self.kernel, read).is_err() {
            println!("acpiserver: the machine is stopping, so the root bridges' memory is not declared");
        }
    }

    /// Initialise the namespace the load kept ([`init`]), and again whatever
    /// a table its initialisation loaded added, until none did; what comes
    /// back is the namespace as it then stands.
    fn initialise(&mut self) -> Option<Names> {
        let aml = self.aml.as_mut()?;
        let names = match Names::read(&aml.interpreter) {
            Ok(names) => names,
            Err(why) => {
                println!("acpiserver: the namespace is not initialised: its walk was refused, {why}");
                return None;
            }
        };
        let defined = aml.interpreter.defined();
        init::initialise(aml, &names, &mut self.census);
        self.settle(defined, names)
    }

    /// Run what each table loaded since `since` added ([`init::loaded`]),
    /// until an initialisation loads nothing more; `names` is the namespace
    /// as it stood at `since`.
    fn settle(&mut self, mut since: u32, mut names: Names) -> Option<Names> {
        let aml = self.aml.as_mut()?;
        while aml.interpreter.defined() > since && !aml.host.stopping {
            names = match Names::read(&aml.interpreter) {
                Ok(names) => names,
                Err(why) => {
                    println!("acpiserver: what a table loaded at run time added is not initialised: the walk was refused, {why}");
                    return None;
                }
            };
            let added = since;
            since = aml.interpreter.defined();
            init::loaded(aml, &names, added, &mut self.census);
        }
        Some(names)
    }

    /// Negotiate `_OSC` ([`osc`]), then initialise what the tables it loaded
    /// added; what comes back is the namespace as it then stands, and its
    /// size is said, after the machine's last table.
    fn negotiate(&mut self, names: Option<Names>) -> Option<Names> {
        let (names, aml) = (names?, self.aml.as_mut()?);
        let before = aml.interpreter.defined();
        self.negotiated = osc::negotiate_all(aml, &names, self.cpu.vendor == cpu::Vendor::Intel);
        let names = self.settle(before, names)?;
        let aml = self.aml.as_ref()?;
        println!(
            "acpiserver: {} table(s) loaded at run time and {} refused; the namespace holds {} objects in {} slots, and the interpreter {} bytes of heap, of the {} it holds between evaluations and the {} while one runs",
            aml.host.loaded,
            aml.host.loads_refused,
            aml.interpreter.defined(),
            aml.interpreter.slots(),
            aml.interpreter.usage().live,
            toyos_aml::MAX_LIVE,
            toyos_aml::MAX_RUNNING
        );
        Some(names)
    }

    /// Hand the kernel each processor's idle states ([`cst`]), once `_OSC`
    /// has said what ToyOS does and the blocks it loaded stand.
    fn declare_idle(&mut self, names: &Names) {
        let Some(aml) = &mut self.aml else { return };
        self.idle = cst::processors(aml, names);
        let mut census = cst::Census::default();
        for processor in &self.idle {
            if cst::declare(aml, self.kernel, processor, self.cpu, &mut census).is_err() {
                return;
            }
        }
        println!("{}", census.line());
    }

    /// `object`'s idle states read again and handed over, on its Notify 0x81.
    fn redeclare_idle(&mut self, object: &str) {
        let (Some(aml), Some(processor)) = (&mut self.aml, self.idle.iter().find(|p| p.path == object)) else { return };
        let mut census = cst::Census::default();
        if cst::declare(aml, self.kernel, processor, self.cpu, &mut census).is_ok() {
            println!("{OWN}{object}'s idle states read again: {}", census.line());
        }
    }

    /// Find the controller, and the buttons where they are control method
    /// devices, in the namespace the load kept; attach the controller to the
    /// AML host ([`devices::attach`]), enable its GPE and drain its backlog. What
    /// comes back is the namespace's batteries and adapters, by path.
    fn arm_controller(&mut self, names: Option<&Names>) -> (Vec<String>, Vec<String>) {
        let (Some(aml), Some(names)) = (&mut self.aml, names) else {
            if !self.served.power_button {
                println!("acpiserver: no control-method power button served: no namespace was kept");
            }
            println!("{CONTROLLER_NONE}no namespace was kept");
            return (Vec::new(), Vec::new());
        };
        let found = devices::find(&mut aml.interpreter, &mut aml.host, names, self.info.gpe0, !self.served.power_button);
        if !self.served.power_button {
            println!("acpiserver: {} control-method power button(s) served, each by the queries' Notify", found.buttons.len());
        }
        aml.host.buttons = found.buttons;
        let sources = (found.batteries, found.adapters);
        let early = aml.host.ec.take();
        let Some(ec) = found.ec else {
            if early.is_some() {
                println!("{CONTROLLER_NONE}the namespace names no controller it serves, and the ECDT's is detached from AML");
            }
            return sources;
        };
        if early.is_some_and(|early| (early.path.as_str(), early.command, early.data) != (ec.path.as_str(), ec.command, ec.data)) {
            println!("acpiserver: the ECDT's embedded controller is not the namespace's, which is served");
        }
        // Its status register, which a read leaves as it was; the data
        // register's read takes a byte, and a port refused later ends the
        // controller then ([`Server::unserve`]).
        match port_in(self.kernel, ec.command) {
            Err(Unmade::Stopping) => return sources,
            Err(Unmade::Refused(why)) => {
                println!("{CONTROLLER_NONE}{why}");
                return sources;
            }
            Ok(_) => {}
        }
        let ec = match devices::attach(aml, ec, names, &mut self.census) {
            Ok(ec) => ec,
            Err(why) => {
                println!("{CONTROLLER_NONE}{why}");
                return sources;
            }
        };
        if aml.host.stopping {
            return sources;
        }
        let n = ec.gpe;
        let (status, enable) = bytes(self.info.gpe0).nth(usize::from(n / 8)).expect("the controller's GPE was bounded by the block");
        self.ports.out8(status, 1 << (n % 8));
        self.ports.out8(enable, self.ports.in8(enable) | 1 << (n % 8));
        println!("{CONTROLLER_SERVED}{n:#x} at {:#x}/{:#x}", ec.command, ec.data);
        self.served.ec_gpe = Some(n);
        self.ec = Some(ec);
        self.drain();
        self.run_queued();
        sources
    }

    /// Find and first read the batteries and adapters the namespace names,
    /// once the controller is armed ([`Power::find`]).
    fn find_power(&mut self, batteries: Vec<String>, adapters: Vec<String>) {
        let Some(aml) = &mut self.aml else { return };
        let census;
        (self.power, census) = Power::find(aml, batteries, adapters);
        if let Some(census) = census {
            println!("{census}");
        }
        self.ended();
    }

    /// Read the batteries and adapters again ([`Power::read`]), and none
    /// more on a machine that is stopping.
    fn read_power(&mut self) {
        let (Some(power), Some(aml)) = (&mut self.power, &mut self.aml) else { return };
        for line in power.read(aml) {
            println!("{line}");
        }
        if aml.host.stopping {
            self.power = None;
        }
        self.ended();
    }

    /// Serve the controller no more where AML's access to it found one of
    /// its ports refused ([`host::Firmware::ended`]).
    fn ended(&mut self) {
        if let Some(why) = self.aml.as_mut().and_then(|aml| aml.host.ended.take()) {
            self.unserve(&why);
        }
    }

    /// Everything one SCI carried, each event served and its status cleared.
    fn take(&mut self) {
        let pm1 = self.info.pm1_event;
        let gpe0: Vec<(u8, u8)> = bytes(self.info.gpe0).map(|(status, enable)| (self.ports.in8(status), self.ports.in8(enable))).collect();
        let events = match sci::events(&self.served, (self.ports.in16(pm1.port), self.ports.in16(pm1.enable())), &gpe0) {
            Ok(events) => events,
            Err(Unserved::Pm1 { status, enable }) => panic!(
                "acpiserver: PM1 status {status:#06x} under enable {enable:#06x} carries an event this server never enabled"
            ),
            Err(Unserved::Gpe(n)) => panic!("acpiserver: GPE {n:#x} is enabled and set, and this server never enabled it"),
        };
        if events.is_empty() {
            self.empty += 1;
            assert!(
                self.empty < sci::EMPTY_SCIS,
                "acpiserver: {} SCIs in a row carried no event this server serves (PM1 {:#06x}, GPE0 {gpe0:x?})",
                self.empty,
                self.ports.in16(pm1.port),
            );
            return;
        }
        self.empty = 0;
        for event in events {
            match event {
                Event::PowerButton => {
                    self.ports.out16(pm1.port, PWRBTN);
                    self.press();
                }
                Event::Ec => {
                    // The controller's GPE is an edge: cleared before the drain,
                    // so an event the drain does not see raises it again.
                    let n = self.served.ec_gpe.expect("an event of the controller's GPE is one this server enabled");
                    let (status, _) = bytes(self.info.gpe0).nth(usize::from(n / 8)).expect("the controller's GPE was bounded by the block");
                    self.ports.out8(status, 1 << (n % 8));
                    self.drain();
                }
                Event::Gpe(n) => self.gpe(n),
            }
        }
        self.run_queued();
        self.served_aml();
    }

    /// Enable each runtime GPE the namespace names ([`gpe::runtime`]), its
    /// status cleared first.
    fn arm_gpes(&mut self, names: &Names) {
        let Some(aml) = &mut self.aml else { return };
        let runtime = gpe::runtime(aml, names, self.info.gpe0, self.served.ec_gpe);
        for &n in runtime.keys() {
            let (status, enable) = bytes(self.info.gpe0).nth(usize::from(n / 8)).expect("a runtime GPE is in GPE0");
            self.ports.out8(status, 1 << (n % 8));
            self.ports.out8(enable, self.ports.in8(enable) | 1 << (n % 8));
        }
        self.served.gpes = runtime;
    }

    /// GPE `n` served by its method (ACPI 6.5 §5.6.4): its enable cleared,
    /// an edge's status cleared, the method run, a level's status cleared,
    /// and its enable set again; a method refused leaves it disabled, said
    /// by name, and served no more.
    fn gpe(&mut self, n: u16) {
        let trigger = self.served.gpes[&n];
        let (status, enable) = bytes(self.info.gpe0).nth(usize::from(n / 8)).expect("a runtime GPE is in GPE0");
        let bit = 1 << (n % 8);
        self.ports.out8(enable, self.ports.in8(enable) & !bit);
        if trigger == sci::Trigger::Edge {
            self.ports.out8(status, bit);
        }
        let method = format!("\\_GPE._{}{n:02X}", if trigger == sci::Trigger::Edge { 'E' } else { 'L' });
        let aml = self.aml.as_mut().expect("a GPE is served only in a namespace");
        let ran = aml.interpreter.evaluate(&mut aml.host, &method, &[]);
        if trigger == sci::Trigger::Level {
            self.ports.out8(status, bit);
        }
        let runs = self.gpe_runs.entry(n).or_insert(0);
        *runs += 1;
        match ran {
            Ok(_) => {
                self.ports.out8(enable, self.ports.in8(enable) | bit);
                if *runs == 1 {
                    println!("acpiserver: GPE {n:#x} taken for the first time; its method ran");
                }
            }
            Err(_) if aml.host.stopping => {}
            Err(why) => {
                self.served.gpes.remove(&n);
                println!("acpiserver: GPE {n:#x}'s method was refused, so the GPE is served no more: {}", aml::kind(&why));
                println!("{OWN}that was {method}, refused {why:x?}");
            }
        }
        self.ended();
    }

    /// What the namespace's evaluations left to serve: each press a Sleep
    /// took, the controller's queries a Sleep found waiting, and each Notify
    /// delivered ([`Server::deliver`]). What these evaluations leave in turn
    /// waits for the next SCI or poll.
    fn served_aml(&mut self) {
        let Some(aml) = &mut self.aml else { return };
        let latched = std::mem::take(&mut aml.host.latched);
        let notified = std::mem::take(&mut aml.host.notified);
        if latched.ec {
            self.drain();
            self.run_queued();
        }
        self.deliver(notified);
        for _ in 0..latched.presses {
            self.press();
        }
    }

    /// Each Notify to its owner (ACPI 6.5 §5.6.6): a battery's or adapter's
    /// status change (0x80) reads the power sources again, and a battery's
    /// information change (0x81) finds them again; `\_SB`'s or a negotiated
    /// processor's 0x08 negotiates its `_OSC` again with the same claim
    /// (§6.2.11); a processor's idle change (0x81, §8.4.1.1) reads its
    /// `_CST` again; its performance or throttling change (0x80, 0x82) is
    /// counted, since nothing reads those states; anything else is served by
    /// nothing and counted.
    fn deliver(&mut self, notified: std::collections::BTreeSet<(String, u64)>) {
        let (mut status, mut info) = (false, false);
        for (object, value) in notified {
            let source = self.sources.0.contains(&object) || self.sources.1.contains(&object);
            let processor = self.negotiated.processors.iter().any(|(path, _)| *path == object);
            let idles = self.idle.iter().any(|p| p.path == object);
            let served = match value {
                0x80 if source => {
                    status = true;
                    "the power sources read again"
                }
                0x81 if self.sources.0.contains(&object) => {
                    info = true;
                    "the power sources found again"
                }
                0x08 if object == "\\_SB_" || processor => {
                    self.renegotiate(&object);
                    "its _OSC negotiated again"
                }
                0x81 if idles => {
                    self.redeclare_idle(&object);
                    "its idle states read again"
                }
                0x80 | 0x82 if processor || idles => "counted: nothing reads the processors' performance or throttling states",
                _ => "served by nothing",
            };
            if self.delivered.see(&format!("{object} {value:#x}")) {
                println!("{OWN}Notify({object}, {value:#x}) for the first time: {served}");
            }
        }
        if info {
            let (batteries, adapters) = self.sources.clone();
            self.find_power(batteries, adapters);
        } else if status {
            self.read_power();
        }
    }

    /// `object`'s `_OSC` negotiated again with its claim.
    fn renegotiate(&mut self, object: &str) {
        let Some(aml) = &mut self.aml else { return };
        let before = aml.interpreter.defined();
        let path = format!("{object}._OSC");
        let answered = if object == "\\_SB_" {
            osc::negotiate(aml, &path, &osc::PLATFORM).map(|answered| format!("{answered:x?}"))
        } else {
            osc::commit(aml, &path, &osc::PROCESSOR).map(|committed| format!("{committed:x?}"))
        };
        match answered {
            Ok(answered) => println!("{OWN}{path} negotiated again: {answered}"),
            Err(why) => println!("{OWN}{path} was refused again: {why}"),
        }
        if let Ok(names) = Names::read(&aml.interpreter) {
            self.settle(before, names);
        }
    }

    /// A press: the supervisor asked to power off once the sessions have
    /// ended, which is answered at once so the next press is served; a press
    /// while that stop is pending forces it, and no other. A stop that ended
    /// without the machine, cancelled or refused, leaves the next press
    /// asking again.
    fn press(&mut self) {
        if self.stop_asked {
            println!("acpiserver: the power button was pressed again, on SCI {}; forcing the stop", self.scis);
            match self.kernel.terminate() {
                Ok(()) => return,
                Err(Refused::NoStopPending) => self.stop_asked = false,
                Err(refused) => panic!("acpiserver: the stop's terminate was refused: {refused:?}"),
            }
        }
        println!("acpiserver: the power button was pressed, on SCI {} of this boot; asking the supervisor to power off", self.scis);
        let Err(refused) = self.kernel.power_off() else {
            self.stop_asked = true;
            return;
        };
        match sci::unstopped(refused, self.power_off) {
            Unstopped::Dropped => toyos::error!("acpiserver: the press is dropped: {}", aml::NO_S5_HANDED.trim_start_matches("acpiserver: ")),
            Unstopped::Another(stop) => toyos::error!("acpiserver: the press is dropped: a {stop:?} is pending"),
            Unstopped::Defect => panic!("acpiserver: the power-off was refused: {refused:?}"),
        }
    }

    /// Take every query the controller has waiting off it, queued for after;
    /// a port the kernel refuses ends the controller by name.
    fn drain(&mut self) {
        match self.take_queries() {
            Ok(()) | Err(Unmade::Stopping) => {}
            Err(Unmade::Refused(why)) => self.unserve(&why),
        }
    }

    fn take_queries(&mut self) -> Result<(), Unmade> {
        let (Some(ec), Some(aml)) = (&self.ec, &mut self.aml) else { return Ok(()) };
        let (command, data, glk) = (ec.command, ec.data, ec.glk);
        let mut taken = 0;
        loop {
            let status = port_in(self.kernel, command)?;
            if status & ec::SCI_EVT == 0 {
                return Ok(());
            }
            let q = aml.host.transaction(command, data, glk, Transaction::query())?;
            if q == 0 {
                return Ok(());
            }
            taken += 1;
            assert!(taken <= QUERIES, "acpiserver: the embedded controller had more than {QUERIES} queries waiting at once (EC_SC {status:#04x})");
            self.queued.push_back(q);
        }
    }

    /// Serve the controller no more: its GPE disabled, what it had waiting
    /// dropped, and why said.
    fn unserve(&mut self, why: &str) {
        let n = self.served.ec_gpe.take().expect("a controller served has its GPE enabled");
        let (_, enable) = bytes(self.info.gpe0).nth(usize::from(n / 8)).expect("the controller's GPE was bounded by the block");
        self.ports.out8(enable, self.ports.in8(enable) & !(1 << (n % 8)));
        self.ec = None;
        if let Some(aml) = &mut self.aml {
            aml.host.ec = None;
        }
        self.queued.clear();
        println!("{CONTROLLER_NONE}{why}");
    }

    /// Each query the drain took, run as its method where a control-method
    /// power button is served ([`run_query`]), and counted.
    fn run_queued(&mut self) {
        while let Some(q) = self.queued.pop_front() {
            let served = run_query(self.aml.as_mut(), self.ec.as_ref(), q);
            self.ended();
            let count = self.counts.entry(q).or_insert(0);
            *count += 1;
            self.counted += 1;
            if *count == 1 {
                let said = match &served {
                    None => UNRUN.into(),
                    Some((Queried::Ran, _)) => "its method ran".into(),
                    Some((Queried::Absent, _)) => "the controller defines no method for it".into(),
                    Some((Queried::Refused(why), _)) => format!("its method did not finish: {why}"),
                };
                println!("acpiserver: embedded controller query {q:#04x} taken for the first time; {said}");
            }
            if served.is_some_and(|(_, presses)| presses != 0) {
                self.press();
            }
        }
    }

    fn log_counts(&mut self) {
        if self.counted == self.logged {
            return;
        }
        self.logged = self.counted;
        let counts: Vec<String> = self.counts.iter().map(|(q, n)| format!("{q:#04x} x{n}")).collect();
        println!("acpiserver: {} SCIs; {}{}", self.scis, acpiserver_api::QUERIES_COUNTED, counts.join(", "));
    }
}

impl<'a, P: Ports> Server<'a, Claim<'a>, P> {
    fn serve(&mut self) -> ! {
        let mut next_count = Instant::now() + COUNTS;
        let mut next_read = Instant::now() + battery::POLL;
        loop {
            if self.power.is_some() && Instant::now() >= next_read {
                self.read_power();
                next_read = Instant::now() + battery::POLL;
            }
            self.kernel.readable(if self.power.is_some() { next_count.min(next_read) } else { next_count });
            if Instant::now() >= next_count {
                self.log_counts();
                next_count = Instant::now() + COUNTS;
            }
            self.latched();
        }
    }
}

/// What a query no method runs for is said to be.
const UNRUN: &str = "served by nothing: no query's method runs where no control-method power button is served";

/// Query `q`'s method run, and the presses its Notifies made, where a
/// control-method power button is served; `None` elsewhere.
fn run_query<K: Kernel>(aml: Option<&mut Aml<'_, K>>, ec: Option<&Ec>, q: u8) -> Option<(Queried, u64)> {
    match (aml, ec) {
        (Some(aml), Some(ec)) if !aml.host.buttons.is_empty() => {
            let queried = devices::query(&mut aml.interpreter, &mut aml.host, ec, q);
            let presses = std::mem::take(&mut aml.host.presses);
            Some((queried, presses))
        }
        _ => None,
    }
}

/// The interpreter's own test encodings of §20.2.
#[cfg(test)]
#[path = "../aml/tests/common/mod.rs"]
mod common;

#[cfg(test)]
mod tests {
    use super::*;
    use toyos_aml::Value;
    use toyos_abi::acpi::{Space, Width};
    use crate::aml::load;
    use crate::aml::tests::{crafted, sealed, CRAFTED_RSDP};
    use crate::devices::tests::{laptop_with_queries, GPE0};
    use crate::host::tests::Scripted;
    use std::cell::RefCell;

    /// The row's registers as bytes a write leaves as written, but those in
    /// `.1`, status registers, where a one written clears its bit (ACPI 6.5
    /// §4.8.4.1) once something has set them.
    #[derive(Default)]
    struct Bytes(RefCell<BTreeMap<u16, u8>>, std::collections::BTreeSet<u16>);

    impl Ports for Bytes {
        fn in8(&self, port: u16) -> u8 {
            *self.0.borrow().get(&port).unwrap_or_else(|| panic!("a read of {port:#x}, which nothing wrote"))
        }

        fn out8(&self, port: u16, byte: u8) {
            let mut bytes = self.0.borrow_mut();
            let written = match bytes.get(&port) {
                Some(&was) if self.1.contains(&port) => was & !byte,
                _ => byte,
            };
            bytes.insert(port, written);
        }

        fn in16(&self, port: u16) -> u16 {
            u16::from_le_bytes([self.in8(port), self.in8(port + 1)])
        }

        fn out16(&self, port: u16, word: u16) {
            let [lo, hi] = word.to_le_bytes();
            self.out8(port, lo);
            self.out8(port + 1, hi);
        }
    }

    /// A controller port the kernel refuses, its status port or, inside a
    /// query's transaction, its data port, ends the controller: its GPE
    /// disabled and no other of the block's, what it had waiting dropped, and
    /// nothing asked of the kernel for it again.
    #[test]
    fn a_controller_port_the_kernel_refuses_ends_the_controller() {
        let (command, data, gpe) = (0x66, 0x62, 3);
        let status = || Access::read(Space::SystemIo, command, Width::Byte);
        for (ports, asked) in [
            (vec![], vec![status()]),
            (
                // A query waits and its answer is ready, at a data port the kernel keeps.
                vec![(command, u64::from(ec::SCI_EVT | ec::OBF))],
                vec![
                    status(),
                    status(),
                    Access::write(Space::SystemIo, command, Width::Byte, 0x84),
                    status(),
                    Access::read(Space::SystemIo, data, Width::Byte),
                ],
            ),
        ] {
            let kernel = Scripted { ports, ..crafted(&sealed(b"DSDT", &[]), &[]) };
            let info = AcpiInfo { rsdp: CRAFTED_RSDP, pm1_event: Block::NONE, gpe0: GPE0, flags: 0, reserved: [0; 3] };
            let mut server = Server::new(&kernel, Bytes::default(), info);
            server.aml = load(&kernel, CRAFTED_RSDP).1;
            kernel.asked.borrow_mut().clear();
            let enable = GPE0.enable();
            server.ports.out8(enable, 0b1000_1001);
            server.served.ec_gpe = Some(gpe);
            server.ec = Some(Ec { path: "\\_SB_.EC0_".into(), command: command as u16, data: data as u16, gpe, queries: Default::default(), glk: false });
            // Taken by an earlier event of the same SCI, and not yet run.
            server.queued.push_back(0x28);
            server.drain();
            assert_eq!(*kernel.asked.borrow(), asked);
            assert_eq!(server.ec, None, "a refused controller is still served");
            assert_eq!(server.served.ec_gpe, None);
            assert_eq!(server.ports.in8(enable), 0b1000_0001, "the controller's GPE is left enabled, or another with it disabled");
            assert!(server.queued.is_empty(), "a refused controller's queries are still run");
            server.drain();
            assert_eq!(kernel.asked.borrow().len(), asked.len(), "a refused controller was asked again");
        }
    }

    /// The controller is attached to AML as it is armed, its `_REG` run and
    /// the battery found through it. A port of it the kernel then refuses,
    /// to AML at the battery's next read or to the server's own drain, ends
    /// the controller for both: detached from AML, unserved, its GPE
    /// disabled, and neither AML nor the drain asks the kernel for it again.
    #[test]
    fn a_controller_port_the_kernel_refuses_to_aml_or_the_drain_ends_it_for_both() {
        for to_aml in [true, false] {
            let kernel = crate::battery::tests::laptop_machine(None);
            let info = AcpiInfo { rsdp: CRAFTED_RSDP, pm1_event: Block::NONE, gpe0: GPE0, flags: FIXED_POWER_BUTTON, reserved: [0; 3] };
            let mut server = Server::new(&kernel, Bytes::default(), info);
            for (_, enable) in bytes(GPE0) {
                server.ports.out8(enable, 0);
            }
            server.aml = load(&kernel, CRAFTED_RSDP).1;
            let names = server.initialise();
            let (batteries, adapters) = server.arm_controller(names.as_ref());
            assert_eq!(server.served.ec_gpe, Some(3));
            assert!(server.aml.as_ref().is_some_and(|aml| aml.host.ec.is_some()), "the served controller is not attached to AML");
            server.find_power(batteries, adapters);
            assert!(server.power.is_some(), "no battery read through the attached controller");
            assert_eq!(server.ec.as_ref().map(|ec| ec.gpe), Some(3));

            // The kernel keeps the controller's ports from here on.
            *kernel.controller.borrow_mut() = None;
            if to_aml {
                server.read_power();
            } else {
                server.drain();
            }
            assert_eq!((server.ec.is_some(), server.served.ec_gpe), (false, None), "refused to AML {to_aml}: the controller is still served");
            assert!(
                server.aml.as_ref().is_some_and(|aml| aml.host.ec.is_none() && aml.host.ended.is_none()),
                "refused to AML {to_aml}: the controller is still attached to AML"
            );
            assert!(bytes(GPE0).all(|(_, enable)| server.ports.in8(enable) == 0), "refused to AML {to_aml}: the controller's GPE is left enabled");
            let asked = kernel.asked.borrow().len();
            server.read_power();
            server.drain();
            let ports = kernel.asked.borrow().iter().skip(asked).filter(|access| access.space == Space::SystemIo as u8).count();
            assert_eq!(ports, 0, "refused to AML {to_aml}: the controller was asked again");
        }
    }

    /// A query waiting as the controller is armed, on a machine whose power
    /// button is a control method device, is drained and its method run only
    /// once the controller is attached to AML and its `_REG` run: the
    /// method's read of the controller's space reaches the controller, and
    /// is not denied as one no controller is attached for.
    #[test]
    fn a_query_waiting_at_arm_runs_its_method_on_the_attached_controller() {
        use crate::battery::tests::{button_laptop_machine, AC, CONNECTED, RD, WR};
        let kernel = button_laptop_machine();
        let info = AcpiInfo { rsdp: CRAFTED_RSDP, pm1_event: Block::NONE, gpe0: GPE0, flags: 0, reserved: [0; 3] };
        let mut server = Server::new(&kernel, Bytes::default(), info);
        for (_, enable) in bytes(GPE0) {
            server.ports.out8(enable, 0);
        }
        server.aml = load(&kernel, CRAFTED_RSDP).1;
        let names = server.initialise();
        server.arm_controller(names.as_ref());
        assert_eq!(server.counts, BTreeMap::from([(0x28, 1)]), "the query waiting was not drained");
        let refused = &server.aml.as_ref().expect("the namespace").host.refused;
        assert!(refused.is_empty(), "{}", refused.counts());
        let done = kernel.controller.borrow().as_ref().expect("a controller").done.clone();
        assert_eq!(done, [(WR, CONNECTED, 1), (0x84, 0, 0), (RD, AC, 0)], "_REG's write, the drain's query, the method's read");
    }

    /// The track's `_REG` bullet and §6.5.4: the controller's `_REG(3, 1)`
    /// runs after every `_INI`, and an `_INI` that writes the controller's
    /// space reaches it only where the ECDT named the controller, which is
    /// attached before any control method runs; without one it is refused,
    /// and the controller is attached by its device after the walk.
    #[test]
    fn the_controllers_reg_runs_after_every_ini_which_reaches_it_through_the_ecdt() {
        use crate::aml::tests::{crafted_with, ecdt};
        use crate::devices::tests::{byte, crs, device, dword, pkg, PNP0C09};
        use crate::ec::tests::Emulated;
        // OperationRegion (ECOR, EmbeddedControl, 0, 0x100); Field (ECOR,
        // ByteAcc) { Offset (0x10), INIB, 8, REGB, 8 }; Method (_INI) { INIB =
        // One }; Method (_REG, 2) { REGB = Arg1 }.
        let region = [&[0x5B, 0x80][..], b"ECOR", &[0x03, 0x00, 0x0B, 0x00, 0x01]].concat();
        let field = [&[0x5B, 0x81][..], &pkg(&[&b"ECOR"[..], &[0x01, 0x00, 0x40, 0x08], b"INIB", &[0x08], b"REGB", &[0x08]].concat())].concat();
        let ini = [&[0x14][..], &pkg(&[&b"_INI"[..], &[0x00, 0x70, 0x01], b"INIB"].concat())].concat();
        let reg = [&[0x14][..], &pkg(&[&b"_REG"[..], &[0x02, 0x70, 0x69], b"REGB"].concat())].concat();
        let ec = device(&[b"_SB_", b"EC0_"], &[dword(b"_HID", PNP0C09), crs(&[0x62, 0x66]), byte(b"_GPE", 3), region, field, ini, reg].concat());
        for with_ecdt in [true, false] {
            let others: Vec<Vec<u8>> = if with_ecdt { vec![ecdt(b"\\_SB.EC0")] } else { vec![] };
            let others: Vec<&[u8]> = others.iter().map(Vec::as_slice).collect();
            let kernel = crafted_with(&sealed(b"DSDT", &ec), &[], &others);
            *kernel.controller.borrow_mut() = Some(Emulated::new([0; 256]));
            let info = AcpiInfo { rsdp: CRAFTED_RSDP, pm1_event: Block::NONE, gpe0: GPE0, flags: FIXED_POWER_BUTTON, reserved: [0; 3] };
            let mut server = Server::new(&kernel, Bytes::default(), info);
            for (_, enable) in bytes(GPE0) {
                server.ports.out8(enable, 0);
            }
            server.aml = load(&kernel, CRAFTED_RSDP).1;
            let names = server.initialise();
            server.arm_controller(names.as_ref());
            let done = kernel.controller.borrow().as_ref().expect("a controller").done.clone();
            let (wr, inib, regb) = (0x81, 0x10, 0x11);
            match with_ecdt {
                true => assert_eq!(done, [(wr, inib, 1), (wr, regb, 1)], "the _INI's write, then _REG's"),
                false => {
                    assert_eq!(done, [(wr, regb, 1)], "only _REG's write");
                    assert_eq!(server.census.refused.counts(), "_INI: denied by this server: a write to EmbeddedControl: no controller is attached to this server's AML x1");
                }
            }
            assert_eq!((server.census.ini, server.census.reg.get(&3).copied()), (u64::from(with_ecdt), Some(1)));
            assert_eq!(server.served.ec_gpe, Some(3));
        }
    }

    /// A platform `_OSC` and an Intel processor's that record what they are
    /// passed in firmware memory and offer bit 0 alone to a query; the
    /// processor's commit loads a block from reserved memory, whose device's
    /// `_INI` sets a byte.
    fn osc_machine(block_type: u8) -> Scripted {
        use crate::common::{arg, buffer, cat, device, else_, field, if_, int, method, name, op_region, pkg, ret, scope, ssdt_named, store, unit};
        const NVS: u64 = 0x7700_0000;
        const BLOCK: u64 = 0x7800_0000;
        let block = ssdt_named("CPU0PSD", &device("\\_SB.NEWD", &method("_INI", 0, &store(&int(0x77), &name("\\NEWI")))));
        let dword = |n: &str| unit(n, 32);
        let nvs = cat(&[
            &op_region("NVSR", 0x00, &int(NVS), &int(0x30)),
            &field("NVSR", 0x03, &["PUU", "PQ1", "PQ2", "PC1", "PC2", "CUU", "CQ1", "CQ2", "CC1", "CC2", "NEWI"].map(dword)),
            &op_region("BLKR", 0x00, &int(BLOCK), &int(block.len() as u64)),
        ]);
        // CreateDWordField (Arg3, n, CDWn) (§19.6.20).
        let created = |at: u64, n: &str| cat(&[&[0x8A], &arg(3), &int(at), &name(n)]);
        let osc = |p: &str, uuid: &[u8; 16], commit: &[u8]| {
            method(
                "_OSC",
                4,
                &cat(&[
                    &created(0, "CDW1"),
                    &created(4, "CDW2"),
                    &if_(&cat(&[&[0x93], &arg(0), &buffer(&int(16), uuid)]), &store(&int(1), &name(&format!("\\{p}UU")))),
                    &if_(
                        &cat(&[&[0x7B], &name("CDW1"), &int(1), &[0x00]]),
                        &cat(&[
                            &store(&name("CDW1"), &name(&format!("\\{p}Q1"))),
                            &store(&name("CDW2"), &name(&format!("\\{p}Q2"))),
                            &store(&int(0x10), &name("CDW1")),
                            &store(&int(1), &name("CDW2")),
                        ]),
                    ),
                    &else_(&cat(&[&store(&name("CDW1"), &name(&format!("\\{p}C1"))), &store(&name("CDW2"), &name(&format!("\\{p}C2"))), commit])),
                    &ret(&arg(3)),
                ]),
            )
        };
        let load = cat(&[&[0x5B, 0x20], &name("\\BLKR"), &[0x60]]);
        // Processor (\_PR.CPU0, 0, 0x410, 6) (ACPI 6.3A §19.6.108).
        let processor = cat(&[&[0x5B, 0x83], &pkg(&cat(&[&name("\\_PR.CPU0"), &[0x00], &0x410u32.to_le_bytes(), &[0x06], &osc("C", &osc::PROCESSOR.uuid, &load)]))]);
        let dsdt = cat(&[&nvs, &scope("\\_SB", &osc("P", &osc::PLATFORM.uuid, &[])), &processor]);
        let mut kernel = crafted(&sealed(b"DSDT", &dsdt), &[]);
        kernel.memory.push((NVS, 10, vec![0; 0x30]));
        kernel.memory.push((BLOCK, block_type, block));
        kernel
    }

    /// §6.2.11: `\_SB._OSC` is queried with what ToyOS claims, nothing, and
    /// committed with what the query granted of it, nothing, though the
    /// firmware offered a bit; on an Intel processor, each processor's is
    /// committed with ToyOS's claim and never queried (302223-007 §2.1). The
    /// block the processor's commit loads from reserved memory loads, and its
    /// device's `_INI` runs. On another vendor's processor its `_OSC` is
    /// not evaluated, and a block in ACPI reclaim memory, which Table 15.6
    /// calls neither reserved nor NVS, is refused and keeps nothing.
    #[test]
    fn osc_commits_only_what_toyos_claims_and_what_its_load_adds_is_initialised() {
        let (on_intel, on_amd) = (cpu::Cpu { vendor: cpu::Vendor::Intel, family: 6 }, cpu::Cpu { vendor: cpu::Vendor::Amd, family: 0x17 });
        for (cpu, block_type, loaded) in [(on_intel, 0, true), (on_amd, 0, false), (on_intel, 9, false)] {
            let intel = cpu == on_intel;
            let kernel = osc_machine(block_type);
            let info = AcpiInfo { rsdp: CRAFTED_RSDP, pm1_event: Block::NONE, gpe0: GPE0, flags: FIXED_POWER_BUTTON, reserved: [0; 3] };
            let mut server = Server::new(&kernel, Bytes::default(), info);
            server.cpu = cpu;
            server.aml = load(&kernel, CRAFTED_RSDP).1;
            let names = server.initialise();
            server.arm_controller(names.as_ref());
            server.negotiate(names).expect("the namespace stands");
            let aml = server.aml.as_mut().expect("the namespace");
            let mut read = |path: &str| match aml.interpreter.evaluate(&mut aml.host, path, &[]) {
                Ok(Value::Integer(v)) => v,
                other => panic!("{path}: {other:?}"),
            };
            let platform: Vec<u64> = ["\\PUU", "\\PQ1", "\\PQ2", "\\PC1", "\\PC2"].map(&mut read).to_vec();
            assert_eq!(platform, [1, 1, 0, 0, 0], "the platform's UUID, the query flag and nothing claimed, then the commit of nothing");
            let processor: Vec<u64> = ["\\CUU", "\\CQ1", "\\CQ2", "\\CC1", "\\CC2"].map(&mut read).to_vec();
            let claimed = u64::from(osc::PROCESSOR.capabilities);
            assert_eq!(processor, if intel { [1, 0, 0, 0, claimed] } else { [0; 5] }, "intel {intel}");
            assert_eq!(read("\\NEWI"), if loaded { 0x77 } else { 0 }, "intel {intel}, block in type {block_type}");
            assert_eq!((aml.host.loaded, aml.host.loads_refused), (u64::from(loaded), u64::from(intel && !loaded)));
            let answered = osc::Answered { query: 0x10, offered: 0, commit: Some((0, 0)) };
            assert_eq!(server.negotiated.platform, Some(Ok(answered)));
            assert_eq!(server.negotiated.processors.len(), usize::from(intel));
            assert_eq!(server.census.ini, u64::from(loaded), "the loaded device's _INI");
        }
    }

    /// Each processor's `_CST` reaches the kernel by the UID the MADT names
    /// it by, a `Processor` object's ProcID, holding the states deeper than
    /// C1 as `_CST` names them: the T14's, as its firmware answers ToyOS's
    /// processor `_OSC`.
    #[test]
    fn each_processors_cst_is_handed_to_the_kernel_by_its_uid() {
        use crate::common::{buffer, cat, def_name, int, name, package, pkg};
        use toyos_abi::acpi::IdleEntry;
        let register = |hint: u64| {
            let mut descriptor = vec![0x82, 12, 0, 0x7F, 1, 2, 1];
            descriptor.extend(hint.to_le_bytes());
            descriptor.extend([0x79, 0x00]);
            buffer(&int(descriptor.len() as u64), &descriptor)
        };
        let state = |hint, kind, latency| package(&[register(hint), int(kind), int(latency), int(0)]);
        let cst = def_name("_CST", &package(&[int(3), state(0x00, 1, 1), state(0x31, 2, 253), state(0x60, 3, 1048)]));
        let processor = |path: &str, id: u8| cat(&[&[0x5B, 0x83], &pkg(&cat(&[&name(path), &[id], &0u32.to_le_bytes(), &[0x00], &cst]))]);
        let kernel = crafted(&sealed(b"DSDT", &cat(&[&processor("\\_PR.CPU0", 2), &processor("\\_PR.CPU1", 5)])), &[]);
        let info = AcpiInfo { rsdp: CRAFTED_RSDP, pm1_event: Block::NONE, gpe0: GPE0, flags: FIXED_POWER_BUTTON, reserved: [0; 3] };
        let mut server = Server::new(&kernel, Bytes::default(), info);
        server.cpu = cpu::Cpu { vendor: cpu::Vendor::Intel, family: 6 };
        server.aml = load(&kernel, CRAFTED_RSDP).1;
        let names = server.initialise().expect("the namespace stands");
        server.declare_idle(&names);
        let declared = kernel.declared.borrow();
        assert_eq!(declared.iter().map(|d| d.processor_uid).collect::<Vec<_>>(), [2, 5]);
        for d in declared.iter() {
            let states: Vec<(u8, u64, u32)> = d.states[..d.count as usize].iter().map(|s| (s.entry, s.address, s.latency_us)).collect();
            assert_eq!(states, [(IdleEntry::Mwait as u8, 0x31, 253), (IdleEntry::Mwait as u8, 0x60, 1048)]);
        }
    }

    /// The POST port as a word field, `\\P80`, and a method `name` that writes
    /// `tag` to it: what the kernel's stand-in was asked to write there is
    /// which methods ran, in order.
    fn posting(name: &str, tag: u64) -> Vec<u8> {
        use crate::common::{int, method, store};
        method(name, 0, &store(&int(tag), &crate::common::name("\\P80")))
    }

    fn post() -> Vec<u8> {
        use crate::common::{cat, field, int, op_region, unit};
        cat(&[&op_region("P80R", 0x01, &int(0x80), &int(2)), &field("P80R", 0x02, &[unit("P80", 16)])])
    }

    fn posted(kernel: &Scripted) -> Vec<u64> {
        kernel.asked.borrow().iter().filter(|access| access.write == 1 && access.space == Space::SystemIo as u8 && access.address == 0x80).map(|access| access.value).collect()
    }

    /// A server over `dsdt`, its namespace loaded and initialised, its GPE0
    /// and PM1 registers all clear.
    fn served(kernel: &Scripted, flags: u16) -> (Server<'_, Scripted, Bytes>, Option<Names>) {
        let info = AcpiInfo { rsdp: CRAFTED_RSDP, pm1_event: Block { port: 0x400, len: 4 }, gpe0: GPE0, flags, reserved: [0; 3] };
        let statuses = bytes(GPE0).map(|(status, _)| status).chain([0x400, 0x401]).collect();
        let mut server = Server::new(kernel, Bytes(RefCell::default(), statuses), info);
        for port in 0x400..0x404 {
            server.ports.0.borrow_mut().insert(port, 0);
        }
        for (status, enable) in bytes(GPE0) {
            server.ports.0.borrow_mut().insert(status, 0);
            server.ports.out8(enable, 0);
        }
        server.aml = load(kernel, CRAFTED_RSDP).1;
        let names = server.initialise();
        (server, names)
    }

    /// §5.6.4: each `\\_GPE._Lxx` and `_Exx` in GPE0 is a runtime GPE and
    /// enabled, but one the `_PRW` of a device other than the buttons and the
    /// PCI host bridge names (§5.6.4.2), the controller's, and one past
    /// GPE0. On its SCI its method runs, its status is cleared and its enable
    /// set again; a method refused leaves it disabled and served no more.
    #[test]
    fn runtime_gpes_are_enabled_and_served_by_their_methods() {
        use crate::common::{cat, def_name, device, int, method, package, scope};
        use crate::devices::tests::PNP0C0C;
        let gpe = scope(
            "\\_GPE",
            &cat(&[
                &posting("_L10", 0x10),
                &posting("_E11", 0x11),
                // A Divide by zero.
                &method("_L12", 0, &[0x78, 0x01, 0x00, 0x00, 0x00]),
                &posting("_L13", 0x13),
                &posting("_L14", 0x14),
                &posting("_L50", 0x50),
                &posting("_L03", 0x03),
            ]),
        );
        let devices = scope(
            "\\_SB",
            &cat(&[
                &device("WAKE", &cat(&[&def_name("_HID", &int(0x0105_D041)), &def_name("_PRW", &package(&[int(0x13), int(3)]))])),
                &device("LID0", &cat(&[&def_name("_HID", &int(u64::from(PNP0C0C))), &def_name("_PRW", &package(&[int(0x14), int(4)]))])),
            ]),
        );
        let mut kernel = crafted(&sealed(b"DSDT", &cat(&[&post(), &gpe, &devices, &crate::devices::tests::laptop(&[])])), &[]);
        kernel.ports = vec![(0x80, 0), (0x66, 0)];
        *kernel.controller.borrow_mut() = Some(crate::ec::tests::Emulated::new([0; 256]));
        let (mut server, names) = served(&kernel, FIXED_POWER_BUTTON);
        server.arm_controller(names.as_ref());
        let names = server.negotiate(names).expect("the namespace stands");
        server.arm_gpes(&names);
        use sci::Trigger::{Edge, Level};
        assert_eq!(server.served.gpes, BTreeMap::from([(0x10, Level), (0x11, Edge), (0x12, Level), (0x14, Level)]));
        let enables: Vec<u8> = bytes(GPE0).map(|(_, enable)| server.ports.in8(enable)).collect();
        assert_eq!(enables, [0b1000, 0, 0b0001_0111, 0], "the controller's GPE 3 and the runtime GPEs");
        let (status, _) = bytes(GPE0).nth(2).expect("byte 2");
        server.ports.0.borrow_mut().insert(status, 0b0000_0111);
        kernel.asked.borrow_mut().clear();
        server.take();
        assert_eq!(posted(&kernel), [0x10, 0x11]);
        assert_eq!(server.ports.in8(status), 0, "a status left set");
        assert_eq!(server.served.gpes.keys().copied().collect::<Vec<_>>(), [0x10, 0x11, 0x14], "a GPE whose method was refused is still served");
        let (_, enable) = bytes(GPE0).nth(2).expect("byte 2");
        assert_eq!(server.ports.in8(enable), 0b0001_0011, "the refused GPE left enabled, or a served one disabled");
    }

    /// §5.6.6: a battery's status change reads the power sources again, a
    /// battery's information change finds them again, `\\_SB`'s 0x08
    /// negotiates its `_OSC` again, and anything else is served by nothing:
    /// each said once and nothing evaluated for it.
    #[test]
    fn a_notify_is_delivered_to_its_owner() {
        let kernel = crate::battery::tests::laptop_machine(None);
        let (mut server, names) = served(&kernel, FIXED_POWER_BUTTON);
        let (batteries, adapters) = server.arm_controller(names.as_ref());
        server.sources = (batteries.clone(), adapters.clone());
        server.find_power(batteries.clone(), adapters);
        let done = || kernel.controller.borrow().as_ref().expect("a controller").done.len();
        let before = done();
        server.deliver([(batteries[0].clone(), 0x80)].into());
        assert!(done() > before, "a battery's status change read no battery");
        let before = done();
        server.deliver([("\\_SB_.NONE".to_string(), 0x80), (batteries[0].clone(), 0x02)].into());
        assert_eq!(done(), before, "a Notify nobody owns evaluated something");
        server.deliver([(batteries[0].clone(), 0x81)].into());
        assert!(done() > before, "a battery's information change found no battery");
        assert_eq!(server.delivered.counts().matches(" x1").count(), 4);

        let kernel = osc_machine(0);
        let (mut server, names) = served(&kernel, FIXED_POWER_BUTTON);
        server.arm_controller(names.as_ref());
        server.negotiate(names).expect("the namespace stands");
        let nvs = || kernel.asked.borrow().iter().filter(|access| access.write == 1 && access.address == 0x7700_0000).count();
        let before = nvs();
        server.deliver([("\\_SB_".to_string(), 0x08)].into());
        assert_eq!(nvs(), before + 2, "the platform's _OSC queried and committed again");
    }

    /// What a Sleep took of the SCI is served after its evaluation: the
    /// controller's queries it found waiting are drained and counted, and
    /// the Sleep served the controller's GPE.
    #[test]
    fn what_a_sleep_took_is_served_after_its_evaluation() {
        use crate::common::{cat, method};
        let sleeps = method("SLP", 0, &[0x5B, 0x22, 0x01]);
        let kernel = crafted(&sealed(b"DSDT", &cat(&[&crate::devices::tests::laptop(&[]), &sleeps])), &[]);
        *kernel.controller.borrow_mut() = Some(crate::ec::tests::Emulated::new([0; 256]));
        let (mut server, names) = served(&kernel, FIXED_POWER_BUTTON);
        server.arm_controller(names.as_ref());
        kernel.controller.borrow_mut().as_mut().expect("a controller").queries.push(0x1C);
        kernel.sleeps.borrow_mut().push_back(sci::Latched { presses: 0, ec: true });
        let aml = server.aml.as_mut().expect("the namespace");
        aml.interpreter.evaluate(&mut aml.host, "\\SLP_", &[]).expect("SLP runs");
        assert_eq!(*kernel.slept.borrow(), [Some(3)], "the Sleep did not serve the controller's GPE");
        assert_eq!(server.counts.get(&0x1C), None);
        server.served_aml();
        assert_eq!(server.counts.get(&0x1C), Some(&1), "the query a Sleep found waiting was not drained");
    }

    /// A press a table's Sleep took during the load powers the machine off
    /// before the namespace is initialised: the `_INI` whose Acquire of the
    /// Global Lock has no bound, while the firmware holds it past
    /// [`host::RELEASE`], is never reached.
    #[test]
    fn a_press_latched_during_the_load_powers_off_before_an_ini_that_waits_on_the_firmware() {
        use crate::common::{cat, device, method, scope};
        use crate::host::tests::POWERED_OFF;
        use crate::host::Take;
        // Sleep (One); Device (\_SB.GLI) { Method (_INI) { Acquire (\_GL, 0xFFFF) Release (\_GL) } }.
        let unbounded = cat(&[&[0x5B, 0x23], b"\\_GL_", &[0xFF, 0xFF], &[0x5B, 0x27], b"\\_GL_"]);
        let dsdt = cat(&[&[0x5B, 0x22, 0x01], &scope("\\_SB", &device("GLI", &method("_INI", 0, &unbounded)))]);
        let kernel = Scripted {
            sleeps: RefCell::new([sci::Latched { presses: 1, ec: false }].into()),
            takes: RefCell::new([Take::Pending].into()),
            ..crafted(&sealed(b"DSDT", &dsdt), &[])
        };
        let info = AcpiInfo { rsdp: CRAFTED_RSDP, pm1_event: Block::NONE, gpe0: GPE0, flags: FIXED_POWER_BUTTON, reserved: [0; 3] };
        let mut server = Server::new(&kernel, Bytes::default(), info);
        let (loaded, kept) = load(&kernel, CRAFTED_RSDP);

        let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| server.loaded(loaded.handed, kept)));

        let why = stopped.err().map(|payload| payload.downcast_ref::<String>().cloned().unwrap_or_default());
        assert_eq!(why.as_deref(), Some(POWERED_OFF), "the press did not power the machine off first");
        assert_eq!(*kernel.takes.borrow(), [Take::Pending], "the _INI took the Global Lock before the press was served");
    }

    /// A press asks a stop that waits for the sessions and is answered at
    /// once, so the next press is served: it forces that stop. A stop that
    /// ended without stopping the machine leaves the press after asking
    /// again, and a stop of another kind pending since is not forced.
    #[test]
    fn a_press_while_the_stop_waits_forces_it() {
        use crate::host::tests::Stop::{PowerOff, Terminate};
        let kernel = Scripted { stops: RefCell::new([Ok(()), Ok(()), Err(Refused::NoStopPending), Ok(())].into()), ..crafted(&[], &[]) };
        let info = AcpiInfo { rsdp: CRAFTED_RSDP, pm1_event: Block::NONE, gpe0: GPE0, flags: FIXED_POWER_BUTTON, reserved: [0; 3] };
        let mut server = Server::new(&kernel, Bytes::default(), info);
        server.press();
        assert_eq!(*kernel.stopped.borrow(), [PowerOff]);
        server.press();
        assert_eq!(*kernel.stopped.borrow(), [PowerOff, Terminate], "a second press did not force the stop");
        server.press();
        assert_eq!(*kernel.stopped.borrow(), [PowerOff, Terminate, Terminate, PowerOff], "a press after the stop ended did not ask again");

        // Cancelled, then a reboot asked by someone else: the press after is
        // refused its terminate, asks its own stop, is refused that, and
        // forces nothing at any press.
        let reboot = Err(Refused::AnotherStopPending(power::Stop::Reboot));
        let kernel = Scripted { stops: RefCell::new([Ok(()), Err(Refused::NoStopPending), reboot, reboot].into()), ..crafted(&[], &[]) };
        let mut server = Server::new(&kernel, Bytes::default(), info);
        for _ in 0..3 {
            server.press();
        }
        assert_eq!(*kernel.stopped.borrow(), [PowerOff, Terminate, PowerOff, PowerOff], "a press forced a stop it never asked");
    }

    /// §6.2.11.1.3: a commit sends what the query offered of the claim, and a
    /// bit claimed that the query did not offer is dropped; what the commit
    /// grants is read of what it sent.
    #[test]
    fn a_commit_sends_only_what_the_query_offered_of_the_claim() {
        let kernel = osc_machine(0);
        let (mut server, _) = served(&kernel, FIXED_POWER_BUTTON);
        let aml = server.aml.as_mut().expect("the namespace");
        let claim = osc::Claim { uuid: osc::PLATFORM.uuid, capabilities: 0b11 };
        let answered = osc::negotiate(aml, "\\_SB_._OSC", &claim);
        assert_eq!(answered, Ok(osc::Answered { query: 0x10, offered: 0b01, commit: Some((0, 0b01)) }));
        let mut read = |path: &str| match aml.interpreter.evaluate(&mut aml.host, path, &[]) {
            Ok(Value::Integer(v)) => v,
            other => panic!("{path}: {other:?}"),
        };
        assert_eq!((read("\\PQ2"), read("\\PC2")), (0b11, 0b01), "the query asks the claim, the commit sends the offer");
    }

    /// The owner's ruling: a machine whose power button is a control method
    /// device and whose namespace was not kept stays in ACPI mode with the
    /// button dead. The server serves neither the button nor the controller,
    /// enables no GPE, asks the kernel nothing, and goes on to serve the
    /// claim rather than end and give it back.
    #[test]
    fn no_namespace_leaves_the_button_and_the_controller_unserved_and_keeps_the_claim() {
        let kernel = Scripted::default();
        let info = AcpiInfo { rsdp: 0, pm1_event: Block::NONE, gpe0: GPE0, flags: 0, reserved: [0; 3] };
        let mut server = Server::new(&kernel, Bytes::default(), info);
        for (_, enable) in bytes(GPE0) {
            server.ports.out8(enable, 0);
        }
        let names = server.initialise();
        server.arm_controller(names.as_ref());
        assert!(!server.served.power_button, "a control-method button is served as the fixed one");
        assert_eq!((server.ec.is_some(), server.served.ec_gpe), (false, None), "a controller is served with no namespace");
        assert!(bytes(GPE0).all(|(_, enable)| server.ports.in8(enable) == 0), "a GPE is enabled with no namespace");
        assert!(kernel.asked.borrow().is_empty(), "the kernel was asked for an access with no namespace");
    }

    /// An AMD laptop's controller and `_Q28` under both kinds of power
    /// button: the method runs, writes the POST port and presses only where
    /// the FADT says the button is a control method device; on a machine
    /// whose button is the fixed one no query's method runs and nothing is
    /// written.
    #[test]
    fn a_querys_method_runs_only_where_a_control_method_button_is_served() {
        for control_method in [false, true] {
            let mut kernel = crafted(&sealed(b"DSDT", &laptop_with_queries()), &[]);
            kernel.ports = vec![(0x80, 0)];
            let (_, kept) = load(&kernel, CRAFTED_RSDP);
            let mut aml = kept.expect("the DSDT loads");
            let names = crate::init::Names::read(&aml.interpreter).expect("the namespace reads");
            let found = devices::find(&mut aml.interpreter, &mut aml.host, &names, GPE0, control_method);
            aml.host.buttons = found.buttons;
            let ec = found.ec.expect("the controller");
            let served = run_query(Some(&mut aml), Some(&ec), 0x28);
            let wrote = kernel.asked.borrow().iter().filter(|access| access.write == 1).count();
            match control_method {
                true => assert_eq!((served, wrote), (Some((Queried::Ran, 1)), 1)),
                false => assert_eq!((served, wrote), (None, 0), "a query's method ran on a machine whose power button is the fixed one"),
            }
        }
    }
}
