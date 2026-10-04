//! The ACPI Machine Language interpreter: a machine's DSDT and SSDTs loaded
//! into one namespace, and its objects evaluated, written from the ACPI
//! Specification 6.5, whose sections the code cites at each rule it encodes.
//!
//! A definition block is firmware's, and so untrusted: whatever its bytes,
//! [`Interpreter::load`] and [`Interpreter::evaluate`] return a value or a
//! named [`Error`], never panic, and never run unbounded — every evaluation
//! is bounded in steps, nesting, object size and time asked to sleep. A load
//! refused leaves the namespace without anything that table created.
//!
//! The library touches no hardware. An operation region's field is read and
//! written through the [`Host`] the caller passes, in SystemMemory,
//! SystemIO, PCI_Config and EmbeddedControl space; an access in any other
//! space is refused as [`Error::Unsupported`], as are `Load`, `LoadTable`
//! and `DataTableRegion`. Only one invocation runs at a time, so a Mutex is
//! never contended and an Event is never signalled by anyone else.
//!
//! The predefined objects are the operating system's (§5.7): `\_OSI` answers
//! as the owner ruled ("Like Windows, not Linux"): yes to every Windows
//! version string Microsoft publishes for `_OSI`, no to anything else;
//! `\_OS` is this system's name and `\_REV` is 2, ACPI 2 or greater with
//! 64-bit integers (§5.7.4).

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
/// What one interpreter holds live across every string, buffer and package,
/// in bytes (`object::Meter`).
pub(crate) const MAX_LIVE: usize = 16 << 20;
/// The bytes of work one step stands for: a step for every this many bytes
/// an operation makes, copies, compares or walks.
pub(crate) const WORK_PER_STEP: usize = 64;
/// The time one evaluation may ask to Sleep, Stall and Wait, together, in µs.
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
    /// Takes (`true`) or gives back (`false`) the firmware's Global Lock
    /// (§5.2.10.1), around a Lock field's access and `\_GL`'s ownership.
    fn global_lock(&mut self, take: bool) -> Result<(), Denied>;
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

/// One machine's namespace, and what loaded it.
pub struct Interpreter {
    ns: Namespace,
    meter: Rc<Meter>,
    /// Set by the DSDT's revision (§19.6.29), for every table after it.
    width: Option<Width>,
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
        let mut ns = Namespace::new();
        let meter = Meter::new();
        let root = ns.root();
        let mut put = |name: &[u8; 4], o: Object| {
            let p = Path { root: true, up: 0, segs: alloc::vec![Seg(*name)] };
            // The root is empty and every name differs.
            let _ = ns.create(root, &p, o);
        };
        for scope in [b"_GPE", b"_PR_", b"_SB_", b"_SI_", b"_TZ_"] {
            put(scope, Object::Scope);
        }
        put(b"_GL_", Object::Mutex(Rc::new(Mutex { sync: 0, held: Cell::new(0), global: true })));
        put(b"_OSI", Object::Method(Rc::new(Method { body: Body::Osi, args: 1, serialized: false, sync: 0 })));
        // The owner's ruling (2026-10-05): "Microsoft Windows NT", as Windows answers.
        if let Ok(os) = meter.bytes(b"Microsoft Windows NT".to_vec()) {
            put(b"_OS_", Object::Str(os));
        }
        put(b"_REV", Object::Int(2));
        Interpreter { ns, meter, width: None }
    }

    /// Loads a DSDT or SSDT (§5.4.2): the DSDT first, then each SSDT. The
    /// table's length and checksum are [`toyos_acpi::Table::open`]'s.
    pub fn load<P: toyos_acpi::Phys>(&mut self, host: &mut dyn Host, table: &toyos_acpi::Table<P>) -> Result<(), Error> {
        let bytes: Vec<u8> = (0..table.len()).map_while(|i| table.byte(i)).collect();
        let (Some(signature), Some(&revision)) = (bytes.first_chunk::<4>(), bytes.get(toyos_acpi::SDT_REVISION)) else {
            return Err(Error::Table("shorter than its header (§5.2.6)"));
        };
        let w = match (signature, self.width) {
            (b"DSDT", None) => Width { bits: if revision < 2 { 32 } else { 64 } },
            (b"DSDT", Some(_)) => return Err(Error::Table("a second DSDT")),
            (b"SSDT", Some(w)) => w,
            (b"SSDT", None) => return Err(Error::Table("an SSDT before the DSDT, whose revision sets every integer's width")),
            _ => return Err(Error::Table("not a DSDT or SSDT (§5.2.11)")),
        };
        let table: Rc<[u8]> = Rc::from(bytes);
        let root = self.ns.root();
        let mut f = Frame::new(root, Vec::new(), table.clone(), 0);
        let mut m = Machine::new(&mut self.ns, host, w, self.meter.clone());
        let mut c = stream::Cursor::new(&table, toyos_acpi::SDT_HEADER_LEN, table.len());
        let r = m.term_list(&mut f, &mut c).and_then(|flow| match flow {
            exec::Flow::Next => Ok(()),
            _ => Err(Error::Rule("a Return, Break or Continue at definition block level")),
        });
        let r = m.finish(r);
        match r {
            Ok(()) => {
                self.width = Some(w);
                Ok(())
            }
            Err(e) => {
                for &id in f.created.iter().rev() {
                    self.ns.remove(id);
                }
                Err(e)
            }
        }
    }

    /// Evaluates the object at an absolute path, written `\_SB.PCI0._STA`: a
    /// method is invoked with `args`, anything else is its value.
    pub fn evaluate(&mut self, host: &mut dyn Host, path: &str, args: &[Value]) -> Result<Value, Error> {
        let w = self.width.ok_or(Error::Table("nothing is loaded"))?;
        let p = Path::absolute(path)?;
        let id = self.ns.resolve(self.ns.root(), &p).ok_or_else(|| Error::NotFound(String::from(path)))?;
        let args = args.iter().map(|a| self.object_of(a, w, 0)).collect::<Result<Vec<_>, _>>()?;
        let mut m = Machine::new(&mut self.ns, host, w, self.meter.clone());
        let r = m.evaluate(id, args).and_then(|o| value_of(&mut m, o, 0));
        m.finish(r)
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
            Value::Reference(path) => {
                let p = Path::absolute(path)?;
                let id = self.ns.resolve(self.ns.root(), &p).ok_or_else(|| Error::NotFound(path.clone()))?;
                Object::Ref(Ref::Node(id))
            }
        })
    }
}

fn value_of(m: &mut Machine<'_>, o: Object, depth: usize) -> Result<Value, Error> {
    if depth > MAX_NESTING {
        return Err(Error::Bound("a package nests deeper than this interpreter copies"));
    }
    Ok(match m.resolve_lazy(o)? {
        Object::Uninit => Value::Uninitialized,
        Object::Int(x) => Value::Integer(x),
        Object::Str(s) => Value::String(s.borrow().clone()),
        Object::Buf(b) => Value::Buffer(b.borrow().clone()),
        Object::Pkg(p) => {
            let elems: Vec<Object> = p.borrow().clone();
            let mut out = Vec::with_capacity(elems.len());
            for e in elems {
                out.push(value_of(m, e, depth + 1)?);
            }
            Value::Package(out)
        }
        Object::Ref(Ref::Node(id)) => Value::Reference(m.ns.path_of(id, None)),
        _ => return Err(Error::Unsupported("a reference to an unnamed object, handed to the caller")),
    })
}
