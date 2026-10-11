//! The ACPI Machine Language interpreter: a machine's DSDT and SSDTs loaded
//! into one namespace, and its objects evaluated, written from the ACPI
//! Specification 6.5, whose sections the code cites at each rule it encodes.
//!
//! A definition block is firmware's, and so untrusted: whatever its bytes,
//! [`Interpreter::load`] and [`Interpreter::evaluate`] return a value or a
//! named [`Error`], never panic, and never run unbounded — every evaluation
//! is bounded in steps, nesting, object size and time asked to sleep, and
//! what an interpreter holds of tables, namespace and objects is bounded in
//! sum, in bytes of heap ([`MAX_LIVE`]). A load refused leaves the namespace
//! without anything that table created, and the interpreter holding what it
//! held before. What the last load or evaluation took of those bounds is
//! [`Interpreter::usage`]'s, a refused one's too.
//!
//! [`Interpreter::walk`] reads the namespace an interpreter holds, and runs
//! nothing of it: which objects a caller then evaluates, and in which order,
//! is the caller's.
//!
//! The library touches no hardware. An operation region's field is read and
//! written through the [`Host`] the caller passes, in SystemMemory,
//! SystemIO, PCI_Config and EmbeddedControl space; an access in any other
//! space is refused as [`Error::Unsupported`], as are `Load`, `LoadTable`
//! and `DataTableRegion`. Only one invocation runs at a time, so a Mutex is
//! never contended and an Event is never signalled by anyone else; `\_GL`'s
//! other owner is the firmware, whose side the [`Host`] waits out.
//!
//! The predefined objects are the operating system's (§5.7), answered as
//! Windows answers them, by the owner's rulings ("Like Windows, not Linux";
//! 2026-10-05 on `\_OS` and on feature groups): `\_OSI` says yes to every
//! Windows version string Microsoft publishes and no to anything else, the
//! ACPI feature groups included, which Windows does not answer ("Windows
//! supports _OSI only for the use of identifying the host version of
//! Windows"); `\_OS` is "Microsoft Windows NT"; `\_REV` is 2, ACPI 2 or
//! greater with 64-bit integers (§5.7.4).
//!
//! An evaluation nests at most [`MAX_DEPTH`] frames of this interpreter, each
//! measured at about 3.4 KiB of stack in a debug build: a caller runs it on a
//! stack of at least 1 MiB.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod exec;
mod field;
mod name;
mod namespace;
mod object;
mod stream;

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;

use exec::{Frame, Machine};
use name::{Path, Seg};
use namespace::Namespace;
use object::{Body, Meter, Method, Mutex, Object, Ref};

pub(crate) use object::Width;

/// The steps one load or evaluation may take: terms, arguments, loop
/// iterations, field access units.
pub(crate) const MAX_STEPS: u64 = 1 << 20;
/// How deep terms, method invocations and field accesses may nest, together.
pub(crate) const MAX_DEPTH: u32 = 256;
/// How deep a package may nest within packages, where it is copied or
/// handed to the caller.
pub(crate) const MAX_NESTING: usize = 64;
/// The largest string, buffer or field, in bytes.
pub(crate) const MAX_BYTES: usize = 1 << 20;
/// The largest package, in elements.
pub(crate) const MAX_ELEMENTS: usize = 1 << 16;
/// The heap one interpreter holds from one load or evaluation to the next,
/// in the bytes its allocations ask for, across every table, namespace node,
/// record, string, buffer and package; what one holds beyond it while it
/// runs is `object::Meter`'s to say.
pub const MAX_LIVE: usize = 16 << 20;
/// The bytes of work one step stands for: a step for every this many bytes
/// an operation makes, copies, compares or walks.
pub(crate) const WORK_PER_STEP: usize = 64;
/// The time one evaluation may ask to Sleep, Stall and Wait, and wait out
/// in Acquires of `\_GL` that timed out, together, in µs.
pub(crate) const MAX_WAIT_US: u64 = 10_000_000;
/// What the Revision opcode answers (§19.6.119): this interpreter's revision.
pub(crate) const REVISION: u64 = 1;

/// The `_OSI` strings answered yes: every one Microsoft publishes for
/// Windows ("How to Identify the Windows Version in ACPI by Using _OSI").
pub(crate) const WINDOWS: &[&str] = &[
    "Windows 2000",
    "Windows 2001",
    "Windows 2001 SP1",
    "Windows 2001.1",
    "Windows 2001 SP2",
    "Windows 2001.1 SP1",
    "Windows 2006",
    "Windows 2006 SP1",
    "Windows 2006.1",
    "Windows 2009",
    "Windows 2012",
    "Windows 2013",
    "Windows 2015",
    "Windows 2016",
    "Windows 2017",
    "Windows 2017.2",
    "Windows 2018",
    "Windows 2018.2",
    "Windows 2019",
    "Windows 2020",
    "Windows 2021",
    "Windows 2022",
];

/// Why a table or an evaluation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The bytes break the AML grammar (§20.2) at this offset into the table.
    Malformed { at: usize, why: &'static str },
    /// A name that must resolve does not (§5.3).
    NotFound(String),
    /// A definition names an object that exists (§5.3: "a name collision ...
    /// is considered fatal").
    Exists(String),
    /// An operand of a type its operator or §19.3.5 refuses.
    Type(&'static str),
    /// A rule of an operator's definition is broken (§19.6).
    Rule(&'static str),
    /// The table's header is refused (§5.2.6).
    Table(&'static str),
    /// The firmware executed Fatal (§19.6.46): the operating system is to
    /// log it and shut down.
    Fatal { kind: u8, code: u32, arg: u64 },
    /// What this interpreter bounds was exceeded.
    Bound(&'static str),
    /// Something the specification defines that this interpreter does not
    /// carry.
    Unsupported(&'static str),
    /// The [`Host`] refused an access.
    Host(String),
    /// A function between a PCI_Config region's device and its host bridge
    /// names no bus below it (§6.5.4), by the registers it answered: a
    /// Header Type that is no PCI-to-PCI bridge's, after which its Secondary
    /// Bus Number was not asked, or a Secondary Bus Number not above `bus`,
    /// the bus the function is on.
    Bridge { segment: u16, bus: u8, device: u8, function: u8, header_type: u8, secondary: Option<u8> },
}

/// A refusal by the [`Host`], saying why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denied(pub String);

/// The width of one access to an operation region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Byte,
    Word,
    DWord,
    QWord,
}

/// Where an access to an operation region lands (Table 5.182).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Address {
    Memory(u64),
    /// A port in the x86 I/O space, which `in` and `out` address in 16 bits.
    Io(u16),
    PciConfig { segment: u16, bus: u8, device: u8, function: u8, offset: u16 },
    EmbeddedControl(u8),
}

/// What the interpreter asks of the operating system.
pub trait Host {
    fn read(&mut self, at: Address, width: Access) -> Result<u64, Denied>;
    fn write(&mut self, at: Address, width: Access, value: u64) -> Result<(), Denied>;
    /// Sleep (§19.6.125): at least `ms` milliseconds, giving up the processor.
    fn sleep(&mut self, ms: u64);
    /// Stall (§19.6.127): at least `us` microseconds, keeping the processor.
    fn stall(&mut self, us: u64);
    /// Timer (§19.6.134): monotonic, in 100 ns units.
    fn timer(&mut self) -> u64;
    /// Notify (§19.6.94) of the object at this absolute path.
    fn notify(&mut self, object: &str, value: u64);
    /// Takes the firmware's Global Lock (§5.2.10.1), for a Lock field's
    /// access or an Acquire of `\_GL` with no TimeoutValue (0xFFFF,
    /// §19.6.2), waiting where the firmware holds it for its release.
    fn global_take(&mut self) -> Result<(), Denied>;
    /// Takes the Global Lock for an Acquire of `\_GL` whose TimeoutValue is
    /// `ms`, waiting where the firmware holds it at most that long: `false`
    /// is the timeout, and nothing taken.
    fn global_take_within(&mut self, ms: u16) -> Result<bool, Denied>;
    /// Gives back the Global Lock a take took.
    fn global_release(&mut self) -> Result<(), Denied>;
}

/// An object as the caller receives or passes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Uninitialized,
    Integer(u64),
    String(Vec<u8>),
    Buffer(Vec<u8>),
    Package(Vec<Value>),
    /// A reference to the named object at this absolute path.
    Reference(String),
}

/// What a load or an evaluation took of its bounds, a refused one as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    /// The steps it took: terms, arguments, loop iterations, field access
    /// units, and the bytes it made, copied, compared or walked.
    pub steps: u64,
    /// The time it asked to Sleep, Stall and Wait, in µs, the request its
    /// limit refused included.
    pub waited_us: u64,
    /// The heap the interpreter held at its end, of [`MAX_LIVE`].
    pub live: usize,
}

/// What a named object is: the types of Table 19.36, a predefined scope,
/// which has none, and a reference a Name holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Uninitialized,
    Integer,
    String,
    Buffer,
    Package,
    FieldUnit,
    Device,
    Event,
    Method,
    Mutex,
    OperationRegion,
    PowerResource,
    /// The Processor object ACPI 6.4 deprecated (ACPI 6.3A §19.6.108).
    Processor,
    ThermalZone,
    BufferField,
    /// `\_SB` and the other scopes of §5.3.1: descended, and no device.
    Scope,
    Reference,
}

/// One object of a [`Walk`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// How many names its path has: 1 directly below the root. Everything
    /// below an object follows it at a greater depth, so a caller leaves a
    /// subtree out by skipping to the next entry no deeper than its top.
    pub depth: u32,
    pub name: [u8; 4],
    pub kind: Kind,
}

/// The namespace read from the root down ([`Interpreter::walk`]). A table
/// chooses how deep it nests, and a path is as long as its depth: an entry
/// is as small at any depth, and is what a caller keeps of a whole
/// namespace, where the paths of one nested as deep as [`MAX_LIVE`] admits
/// are gigabytes.
pub struct Walk<'a>(namespace::Walk<'a>);

impl Walk<'_> {
    /// The absolute path of the entry last returned, as
    /// [`Interpreter::evaluate`] takes it.
    pub fn path(&self) -> &str {
        self.0.path()
    }
}

impl Iterator for Walk<'_> {
    type Item = Entry;

    fn next(&mut self) -> Option<Entry> {
        let (depth, seg, object) = self.0.step()?;
        let kind = match object {
            Object::Uninit => Kind::Uninitialized,
            Object::Int(_) => Kind::Integer,
            Object::Str(_) => Kind::String,
            Object::Buf(_) => Kind::Buffer,
            Object::Pkg(_) => Kind::Package,
            Object::Field(_) => Kind::FieldUnit,
            Object::BufField(_) => Kind::BufferField,
            Object::Ref(_) | Object::Lazy(_) => Kind::Reference,
            Object::Scope => Kind::Scope,
            Object::Device => Kind::Device,
            Object::Processor => Kind::Processor,
            Object::ThermalZone => Kind::ThermalZone,
            Object::PowerResource => Kind::PowerResource,
            Object::Method(_) => Kind::Method,
            Object::Mutex(_) => Kind::Mutex,
            Object::Event(_) => Kind::Event,
            Object::Region(_) => Kind::OperationRegion,
        };
        Some(Entry { depth, name: seg.0, kind })
    }
}

/// One machine's namespace, and what loaded it.
pub struct Interpreter {
    ns: Namespace,
    meter: Rc<Meter>,
    /// Set by the DSDT's revision (§19.6.29), for every table after it.
    width: Option<Width>,
    last: Usage,
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl Interpreter {
    /// An empty namespace holding the predefined scopes (§5.3.1) and objects
    /// (§5.7).
    pub fn new() -> Self {
        let meter = Meter::new();
        let mut ns = Namespace::new(meter.clone());
        let root = ns.root();
        let mut put = |name: &[u8; 4], o: Object| {
            let p = Path { root: true, up: 0, segs: alloc::vec![Seg(*name)] };
            ns.create(root, &p, o, &mut || Ok(())).expect("the root is empty and every predefined name differs");
        };
        for scope in [b"_GPE", b"_PR_", b"_SB_", b"_SI_", b"_TZ_"] {
            put(scope, Object::Scope);
        }
        let held = "an empty meter holds the predefined objects";
        put(b"_GL_", Object::Mutex(meter.hold(Mutex { sync: 0, held: Cell::new(0), global: true }).expect(held)));
        put(b"_OSI", Object::Method(meter.hold(Method { body: Body::Osi, args: 1, serialized: false, sync: 0 }).expect(held)));
        // The owner's ruling (2026-10-05): "Microsoft Windows NT", as Windows answers.
        let os = meter.bytes(b"Microsoft Windows NT".to_vec()).expect(held);
        put(b"_OS_", Object::Str(os));
        put(b"_REV", Object::Int(2));
        let last = Usage { live: meter.live(), ..Usage::default() };
        Interpreter { ns, meter, width: None, last }
    }

    /// Loads a DSDT or SSDT (§5.4.2): the DSDT first, then each SSDT. The
    /// table's header, length and checksum are [`toyos_acpi::Table::open`]'s.
    pub fn load<P: toyos_acpi::Phys>(&mut self, host: &mut dyn Host, table: &toyos_acpi::Table<P>) -> Result<(), Error> {
        self.last = Usage::default();
        let r = self.load_in(host, table);
        self.last.live = self.meter.live();
        r
    }

    fn load_in<P: toyos_acpi::Phys>(&mut self, host: &mut dyn Host, table: &toyos_acpi::Table<P>) -> Result<(), Error> {
        let bytes: Vec<u8> = (0..table.len()).map_while(|i| table.byte(i)).collect();
        let w = match (&bytes[..4], self.width) {
            (b"DSDT", None) => Width { bits: if bytes[toyos_acpi::SDT_REVISION] < 2 { 32 } else { 64 } },
            (b"DSDT", Some(_)) => return Err(Error::Table("a second DSDT")),
            (b"SSDT", Some(w)) => w,
            (b"SSDT", None) => return Err(Error::Table("an SSDT before the DSDT, whose revision sets every integer's width")),
            _ => return Err(Error::Table("not a DSDT or SSDT (§5.2.11)")),
        };
        // Held for as long as a method it defines refers to it.
        let table = self.meter.bytes(bytes)?;
        let (root, made) = (self.ns.root(), self.ns.mark());
        let mut f = Frame::new(root, Vec::new(), table.clone(), 0);
        let mut m = Machine::new(&mut self.ns, host, w, self.meter.clone());
        let bytes = table.borrow();
        let mut c = stream::Cursor::new(&bytes, toyos_acpi::SDT_HEADER_LEN, bytes.len());
        let r = m.term_list(&mut f, &mut c).and_then(|flow| match flow {
            exec::Flow::Next => Ok(()),
            _ => Err(Error::Rule("a Return, Break or Continue at definition block level")),
        });
        let r = m.finish(r, &mut self.last);
        if r.is_ok() {
            self.width = Some(w);
        } else {
            self.ns.unwind(made);
        }
        r
    }

    /// Evaluates the object at an absolute path, written `\_SB.PCI0._STA`: a
    /// method is invoked with `args`, anything else is its value.
    pub fn evaluate(&mut self, host: &mut dyn Host, path: &str, args: &[Value]) -> Result<Value, Error> {
        self.last = Usage::default();
        let r = self.evaluate_in(host, path, args);
        self.last.live = self.meter.live();
        r
    }

    fn evaluate_in(&mut self, host: &mut dyn Host, path: &str, args: &[Value]) -> Result<Value, Error> {
        let w = self.width.ok_or(Error::Table("nothing is loaded"))?;
        let id = self.named(path)?;
        let args = args.iter().map(|a| self.object_of(a, w, 0)).collect::<Result<Vec<_>, _>>()?;
        let meter = self.meter.clone();
        let mut m = Machine::new(&mut self.ns, host, w, meter.clone());
        let mut handed = 0;
        let r = m.evaluate(id, args).and_then(|o| value_of(&mut m, &meter, &mut handed, o, 0));
        // The value is the caller's from here, and no longer this interpreter's.
        meter.give(handed);
        m.finish(r, &mut self.last)
    }

    /// What the last load or evaluation took, and what the interpreter held
    /// at its end.
    pub fn usage(&self) -> Usage {
        self.last
    }

    /// Every object the loaded tables and this interpreter defined, read-only
    /// and without evaluating any: each after the object it is in, siblings
    /// in the order they were declared, a table's after those of the tables
    /// loaded before it. An Alias is not among them; what it names is, where
    /// it was defined. The walk is held against [`MAX_LIVE`] while it lasts,
    /// and refused where that has no room for it.
    pub fn walk(&self) -> Result<Walk<'_>, Error> {
        self.ns.walk().map(Walk)
    }

    fn object_of(&self, v: &Value, w: Width, depth: usize) -> Result<Object, Error> {
        if depth > MAX_NESTING {
            return Err(Error::Bound("a package nests deeper than this interpreter copies"));
        }
        Ok(match v {
            Value::Uninitialized => Object::Uninit,
            Value::Integer(x) => Object::Int(x & w.ones()),
            Value::String(s) if s.contains(&0) => return Err(Error::Type("a String argument holds a NUL")),
            Value::String(s) => Object::Str(self.meter.bytes(s.clone())?),
            Value::Buffer(b) => Object::Buf(self.meter.bytes(b.clone())?),
            Value::Package(p) => Object::Pkg(
                self.meter.list(p.iter().map(|e| self.object_of(e, w, depth + 1)).collect::<Result<_, _>>()?)?,
            ),
            Value::Reference(path) => Object::Ref(Ref::Node(self.named(path)?)),
        })
    }

    /// The object at an absolute path the caller wrote, whose length is the
    /// caller's and costs no evaluation a step.
    fn named(&self, path: &str) -> Result<namespace::NodeId, Error> {
        let p = Path::absolute(path)?;
        self.ns.resolve(self.ns.root(), &p, &mut || Ok(()))?.ok_or_else(|| Error::NotFound(String::from(path)))
    }
}

/// `n` bytes of the value an evaluation is building for its caller, held
/// against the meter until it is handed over, and added to `handed`.
fn hand(meter: &Meter, handed: &mut usize, n: usize) -> Result<(), Error> {
    meter.take(n)?;
    *handed += n;
    Ok(())
}

/// An object as the caller receives it. A package element that names an
/// object is resolved here, to a copy as large as its table chose, once for
/// every element that names it: the value's bytes are held against the meter
/// while it is built, and a package's elements while they are walked.
fn value_of(m: &mut Machine<'_>, meter: &Meter, handed: &mut usize, o: Object, depth: usize) -> Result<Value, Error> {
    if depth > MAX_NESTING {
        return Err(Error::Bound("a package nests deeper than this interpreter copies"));
    }
    Ok(match m.resolve_lazy(o)? {
        Object::Uninit => Value::Uninitialized,
        Object::Int(x) => Value::Integer(x),
        Object::Str(s) => {
            hand(meter, handed, s.borrow().len())?;
            Value::String(s.borrow().clone())
        }
        Object::Buf(b) => {
            hand(meter, handed, b.borrow().len())?;
            Value::Buffer(b.borrow().clone())
        }
        Object::Pkg(p) => {
            let count = p.borrow().len();
            let walked = count * object::ELEMENT;
            hand(meter, handed, walked + count * core::mem::size_of::<Value>())?;
            let elems: Vec<Object> = p.borrow().clone();
            let mut out = Vec::with_capacity(count);
            for e in elems {
                out.push(value_of(m, meter, handed, e, depth + 1)?);
            }
            meter.give(walked);
            *handed -= walked;
            Value::Package(out)
        }
        Object::Ref(Ref::Node(id)) => {
            let path = m.path_of(id, None)?;
            hand(meter, handed, path.capacity())?;
            Value::Reference(path)
        }
        _ => return Err(Error::Unsupported("a reference to an unnamed object, handed to the caller")),
    })
}
