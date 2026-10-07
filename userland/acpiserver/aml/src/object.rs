//! What the namespace and a method's locals hold, and the data conversions
//! of §19.3.5.7, which every implicit and explicit conversion applies.
//!
//! Strings, buffers and packages are shared, not copied, while a term
//! evaluates: a field created over a buffer (§19.6.21) and a reference made
//! by Index (§19.6.62) see the object they were made from. A store copies
//! (§19.3.5.8), and so does a return (§19.6.118).
//!
//! Every string, buffer and package, every loaded table a method still runs
//! from, every namespace node and every package element's name not yet
//! defined is held against its interpreter's [`Meter`] from its making to its
//! end, so what one interpreter holds live is bounded in sum.
//!
//! A reference to a LocalX or ArgX does not hold it: the frame alone does,
//! and once its method exits the reference names nothing. A reference to a
//! package element holds its package, and lives only in a LocalX or ArgX.
//! Neither kind enters a package or a named object, so no reference owns
//! another, no chain of them forms, and none is part of a cycle.

use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use crate::field::{BufField, Field, Region};
use crate::name::Path;
use crate::namespace::NodeId;
use crate::{Error, MAX_BYTES, MAX_ELEMENTS, MAX_LIVE};

pub(crate) type Bytes = Rc<Data>;
pub(crate) type Elems = Rc<List>;
pub(crate) type Slot = Rc<RefCell<Object>>;

/// What one interpreter holds live, in bytes: a string's, buffer's or
/// table's length, a package's elements at [`ELEMENT`] bytes each, a
/// namespace node at the size of one, and an element's name not yet defined
/// at its own size and its segments'. Whatever a table sizes is counted;
/// what is not is bounded by a constant for each node and element: a field's
/// or method's own record, a node's entry among its parent's children, a
/// shared object's counts, the allocator's overhead.
pub(crate) struct Meter {
    live: Cell<usize>,
}

/// The bytes a package element is held at.
pub(crate) const ELEMENT: usize = core::mem::size_of::<Object>();

impl Meter {
    pub(crate) fn new() -> Rc<Meter> {
        Rc::new(Meter { live: Cell::new(0) })
    }

    pub(crate) fn take(&self, n: usize) -> Result<(), Error> {
        let live = self.live.get().checked_add(n).filter(|&l| l <= MAX_LIVE);
        self.live.set(live.ok_or(Error::Bound("more held live than one interpreter holds"))?);
        Ok(())
    }

    pub(crate) fn give(&self, n: usize) {
        self.live.set(self.live.get().checked_sub(n).expect("the meter gives back only what it took"));
    }

    /// A string's, buffer's or table's bytes, refused past [`MAX_BYTES`].
    pub(crate) fn bytes(self: &Rc<Self>, v: Vec<u8>) -> Result<Bytes, Error> {
        bounded(v.len())?;
        self.take(v.len())?;
        Ok(Rc::new(Data { v: RefCell::new(v), meter: self.clone() }))
    }

    /// A package's elements, refused past [`MAX_ELEMENTS`].
    pub(crate) fn list(self: &Rc<Self>, v: Vec<Object>) -> Result<Elems, Error> {
        counted(v.len())?;
        self.take(v.len() * ELEMENT)?;
        Ok(Rc::new(List { v: RefCell::new(v), meter: self.clone() }))
    }

    /// A package element's name that `scope` does not resolve yet.
    pub(crate) fn unresolved(self: &Rc<Self>, path: Path, scope: NodeId) -> Result<Rc<Unresolved>, Error> {
        self.take(Unresolved::held(&path))?;
        Ok(Rc::new(Unresolved { path, scope, meter: self.clone() }))
    }
}

/// A string's or buffer's bytes, held against a [`Meter`].
pub(crate) struct Data {
    v: RefCell<Vec<u8>>,
    meter: Rc<Meter>,
}

impl Data {
    pub(crate) fn borrow(&self) -> core::cell::Ref<'_, Vec<u8>> {
        self.v.borrow()
    }

    /// The bytes in place, to change: a slice, which cannot be resized.
    pub(crate) fn bits(&self) -> core::cell::RefMut<'_, [u8]> {
        core::cell::RefMut::map(self.v.borrow_mut(), Vec::as_mut_slice)
    }

    pub(crate) fn replace(&self, n: Vec<u8>) -> Result<(), Error> {
        bounded(n.len())?;
        self.meter.take(n.len())?;
        let old = self.v.replace(n);
        self.meter.give(old.len());
        Ok(())
    }
}

impl Drop for Data {
    fn drop(&mut self) {
        self.meter.give(self.v.get_mut().len());
    }
}

/// A package's elements, held against a [`Meter`].
pub(crate) struct List {
    v: RefCell<Vec<Object>>,
    meter: Rc<Meter>,
}

impl List {
    pub(crate) fn borrow(&self) -> core::cell::Ref<'_, Vec<Object>> {
        self.v.borrow()
    }

    /// One element, replaced; the count stays.
    pub(crate) fn set(&self, i: usize, o: Object) -> Result<(), Error> {
        let mut v = self.v.borrow_mut();
        *v.get_mut(i).ok_or(Error::Rule("an Index reference past its package's end"))? = o;
        Ok(())
    }

    pub(crate) fn replace(&self, n: Vec<Object>) -> Result<(), Error> {
        counted(n.len())?;
        self.meter.take(n.len() * ELEMENT)?;
        let old = self.v.replace(n);
        self.meter.give(old.len() * ELEMENT);
        Ok(())
    }
}

impl Drop for List {
    fn drop(&mut self) {
        self.meter.give(self.v.get_mut().len() * ELEMENT);
    }
}

/// A package element named by a path that did not resolve when the package
/// was evaluated, held against a [`Meter`]: a path is as long as its table
/// wrote it.
pub(crate) struct Unresolved {
    pub(crate) path: Path,
    pub(crate) scope: NodeId,
    meter: Rc<Meter>,
}

impl Unresolved {
    fn held(path: &Path) -> usize {
        core::mem::size_of::<Unresolved>() + core::mem::size_of_val(path.segs.as_slice())
    }
}

impl Drop for Unresolved {
    fn drop(&mut self) {
        self.meter.give(Self::held(&self.path));
    }
}

pub(crate) fn bounded(len: usize) -> Result<(), Error> {
    if len > MAX_BYTES { Err(Error::Bound("an object larger than this interpreter holds")) } else { Ok(()) }
}

pub(crate) fn counted(len: usize) -> Result<(), Error> {
    if len > MAX_ELEMENTS { Err(Error::Bound("a package larger than this interpreter holds")) } else { Ok(()) }
}

#[derive(Clone)]
pub(crate) enum Object {
    Uninit,
    Int(u64),
    /// ASCII characters, no terminator.
    Str(Bytes),
    Buf(Bytes),
    Pkg(Elems),
    Field(Rc<Field>),
    BufField(Rc<BufField>),
    Ref(Ref),
    /// A predefined scope such as `\_SB` (§5.3.1), typeless (§19.6.96).
    Scope,
    Device,
    /// The Processor object ACPI 6.4 deprecated (ACPI 6.3A §19.6.108).
    Processor,
    ThermalZone,
    PowerResource,
    Method(Rc<Method>),
    Mutex(Rc<Mutex>),
    /// An Event's pending signal count (§19.6.147).
    Event(Rc<Cell<u64>>),
    Region(Rc<Region>),
    /// A package element whose name is resolved when read (§19.6.101).
    Lazy(Rc<Unresolved>),
}

/// An object reference (§19.6.113, §19.6.62).
#[derive(Clone)]
pub(crate) enum Ref {
    Node(NodeId),
    /// A method's LocalX or ArgX (§19.3.5.8.1: "RefOf (ArgX) returns a
    /// reference to ArgX"), which its frame alone holds.
    Slot(Weak<RefCell<Object>>),
    Elem(Elems, usize),
    BufField(Rc<BufField>),
}

/// The LocalX or ArgX a reference names, while its method runs.
pub(crate) fn slot_of(s: &Weak<RefCell<Object>>) -> Result<Slot, Error> {
    s.upgrade().ok_or_else(|| Error::NotFound(String::from("a LocalX or ArgX its method's exit destroyed")))
}

pub(crate) enum Body {
    Aml { table: Bytes, start: usize, end: usize },
    /// `\_OSI`, which the operating system implements (§5.7.2).
    Osi,
}

pub(crate) struct Method {
    pub(crate) body: Body,
    pub(crate) args: u8,
    pub(crate) serialized: bool,
    pub(crate) sync: u8,
}

pub(crate) struct Mutex {
    pub(crate) sync: u8,
    pub(crate) held: Cell<u32>,
    /// `\_GL`, which also takes the firmware's Global Lock (§5.7.1).
    pub(crate) global: bool,
}

impl Object {
    /// Whether this is a reference that may live only in a LocalX or ArgX
    /// (the module header).
    pub(crate) fn frame_bound(&self) -> bool {
        matches!(self, Object::Ref(Ref::Slot(_) | Ref::Elem(..)))
    }

    /// The value ObjectType returns (§19.6.96, Table 19.36; a Processor's 12
    /// from ACPI 6.3A's), a reference's being its target's and found by the
    /// caller.
    pub(crate) fn type_code(&self) -> u64 {
        match self {
            Object::Uninit | Object::Scope | Object::Lazy(_) | Object::Ref(_) => 0,
            Object::Int(_) => 1,
            Object::Str(_) => 2,
            Object::Buf(_) => 3,
            Object::Pkg(_) => 4,
            Object::Field(_) => 5,
            Object::Device => 6,
            Object::Event(_) => 7,
            Object::Method(_) => 8,
            Object::Mutex(_) => 9,
            Object::Region(_) => 10,
            Object::PowerResource => 11,
            Object::Processor => 12,
            Object::ThermalZone => 13,
            Object::BufField(_) => 14,
        }
    }
}

/// The integer width a definition block's DSDT revision gives every integer
/// (§19.6.29: "If the ComplianceRevision is less than 2, all integers are
/// restricted to 32 bits").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Width {
    pub(crate) bits: u32,
}

impl Width {
    pub(crate) fn ones(self) -> u64 {
        if self.bits == 32 { u64::from(u32::MAX) } else { u64::MAX }
    }

    pub(crate) fn bytes(self) -> usize {
        (self.bits / 8) as usize
    }

    pub(crate) fn bool(self, b: bool) -> u64 {
        if b { self.ones() } else { 0 }
    }

    pub(crate) fn le(self, v: u64) -> Vec<u8> {
        v.to_le_bytes()[..self.bytes()].to_vec()
    }

    /// Integer from Buffer (Table 19.7): least significant byte first, up to
    /// the integer's width; a zero-length buffer is not allowed.
    pub(crate) fn int_of_bytes(self, b: &[u8]) -> Result<u64, Error> {
        if b.is_empty() {
            return Err(Error::Type("a zero-length buffer converts to no integer (Table 19.7)"));
        }
        Ok(b.iter().take(self.bytes()).enumerate().fold(0, |v, (i, &x)| v | u64::from(x) << (8 * i)))
    }

    /// Integer from String (Table 19.7): hexadecimal digits from the first,
    /// most significant, up to the first that is not one or the integer's
    /// width in digits; a zero-length string is not allowed, nor a `0x`.
    pub(crate) fn int_of_str(self, s: &[u8]) -> Result<u64, Error> {
        if s.is_empty() {
            return Err(Error::Type("a zero-length string converts to no integer (Table 19.7)"));
        }
        let digits = s.iter().take(self.bytes() * 2).map_while(|&c| char::from(c).to_digit(16));
        Ok(digits.fold(0, |v, d| v << 4 | u64::from(d)))
    }

    /// String from Integer (Table 19.7): the whole integer in hexadecimal,
    /// 8 or 16 characters.
    pub(crate) fn hex(self, v: u64) -> Vec<u8> {
        let n = self.bytes() * 2;
        (0..n).rev().map(|i| HEX[((v >> (4 * i)) & 0xF) as usize]).collect()
    }
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// The Integer a data object converts to (Table 19.7).
pub(crate) fn to_int(o: &Object, w: Width) -> Result<u64, Error> {
    match o {
        Object::Int(v) => Ok(*v & w.ones()),
        Object::Str(s) => w.int_of_str(&s.borrow()),
        Object::Buf(b) => w.int_of_bytes(&b.borrow()),
        Object::Uninit => Err(Error::Type("an uninitialized object is used as a source (Table 19.6)")),
        _ => Err(Error::Type("an operand that converts to no integer (Table 19.6)")),
    }
}

/// The Buffer a data object converts to (Table 19.7): an integer's 4 or 8
/// bytes, a string's characters with its terminator.
pub(crate) fn to_buf(o: &Object, w: Width) -> Result<Vec<u8>, Error> {
    match o {
        Object::Int(v) => Ok(w.le(*v)),
        Object::Str(s) => {
            let s = s.borrow();
            bounded(s.len() + 1)?;
            let mut b = s.clone();
            if !s.is_empty() {
                b.push(0);
            }
            Ok(b)
        }
        Object::Buf(b) => Ok(b.borrow().clone()),
        Object::Uninit => Err(Error::Type("an uninitialized object is used as a source (Table 19.6)")),
        _ => Err(Error::Type("an operand that converts to no buffer (Table 19.6)")),
    }
}

/// The String a data object converts to (Table 19.7): an integer in
/// hexadecimal, a buffer as two-digit hexadecimal numbers separated by
/// spaces.
pub(crate) fn to_str(o: &Object, w: Width) -> Result<Vec<u8>, Error> {
    match o {
        Object::Int(v) => Ok(w.hex(*v)),
        Object::Str(s) => Ok(s.borrow().clone()),
        Object::Buf(b) => {
            bounded(b.borrow().len().saturating_mul(3))?;
            Ok(joined(&b.borrow(), b' ', hex2))
        }
        Object::Uninit => Err(Error::Type("an uninitialized object is used as a source (Table 19.6)")),
        _ => Err(Error::Type("an operand that converts to no string (Table 19.6)")),
    }
}

pub(crate) fn hex2(x: u8, out: &mut Vec<u8>) {
    out.extend([HEX[usize::from(x >> 4)], HEX[usize::from(x & 0xF)]]);
}

pub(crate) fn joined(b: &[u8], sep: u8, each: fn(u8, &mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len() * 4);
    for (i, &x) in b.iter().enumerate() {
        if i > 0 {
            out.push(sep);
        }
        each(x, &mut out);
    }
    out
}

pub(crate) fn decimal(v: u64) -> Vec<u8> {
    alloc::format!("{v}").into_bytes()
}

/// Bytes fitted to `len` bytes: truncated, or zero-extended.
pub(crate) fn fit(mut b: Vec<u8>, len: usize) -> Vec<u8> {
    b.resize(len, 0);
    b
}
