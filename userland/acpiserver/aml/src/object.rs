//! What the namespace and a method's locals hold, and the data conversions
//! of §19.3.5.7, which every implicit and explicit conversion applies.
//!
//! Strings, buffers and packages are shared, not copied, while a term
//! evaluates: a field created over a buffer (§19.6.21) and a reference made
//! by Index (§19.6.62) see the object they were made from. A store copies
//! (§19.3.5.8), and so does a return (§19.6.118).

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use crate::field::{BufField, Field, Region};
use crate::name::Path;
use crate::namespace::NodeId;
use crate::{Error, MAX_BYTES, MAX_NESTING};

pub(crate) type Bytes = Rc<RefCell<Vec<u8>>>;
pub(crate) type Elems = Rc<RefCell<Vec<Object>>>;
pub(crate) type Slot = Rc<RefCell<Object>>;

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
    ThermalZone,
    PowerResource,
    Method(Rc<Method>),
    Mutex(Rc<Mutex>),
    /// An Event's pending signal count (§19.6.147).
    Event(Rc<Cell<u64>>),
    Region(Rc<Region>),
    /// A package element named by a path that did not resolve when the
    /// package was evaluated; it is resolved when read (§19.6.101).
    Lazy(Rc<(Path, NodeId)>),
}

/// An object reference (§19.6.113, §19.6.62).
#[derive(Clone)]
pub(crate) enum Ref {
    Node(NodeId),
    /// A method's LocalX or ArgX (§19.3.5.8.1: "RefOf (ArgX) returns a
    /// reference to ArgX").
    Slot(Slot),
    Elem(Elems, usize),
    BufField(Rc<BufField>),
}

pub(crate) enum Body {
    Aml { table: Rc<[u8]>, start: usize, end: usize },
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

pub(crate) fn bytes(v: Vec<u8>) -> Bytes {
    Rc::new(RefCell::new(v))
}

impl Object {
    pub(crate) fn str(v: Vec<u8>) -> Object {
        Object::Str(bytes(v))
    }

    pub(crate) fn buf(v: Vec<u8>) -> Object {
        Object::Buf(bytes(v))
    }

    /// The value ObjectType returns (§19.6.96, Table 19.36), a reference's
    /// being its target's and found by the caller.
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
            Object::ThermalZone => 13,
            Object::BufField(_) => 14,
        }
    }
}

/// A copy of an object for a store (§19.3.5.8): data is duplicated, anything
/// else is the same object again. Bounded in size and in nesting, both of
/// which a table can grow without limit by storing a package into itself.
pub(crate) fn copy(o: &Object) -> Result<Object, Error> {
    let mut weight = 0usize;
    copy_in(o, &mut weight, 0)
}

fn copy_in(o: &Object, weight: &mut usize, depth: usize) -> Result<Object, Error> {
    if depth > MAX_NESTING {
        return Err(Error::Bound("a package nests deeper than this interpreter copies"));
    }
    let mut weigh = |n: usize| {
        *weight = weight.saturating_add(n);
        if *weight > MAX_BYTES { Err(Error::Bound("an object larger than this interpreter holds")) } else { Ok(()) }
    };
    Ok(match o {
        Object::Str(s) => {
            weigh(s.borrow().len())?;
            Object::str(s.borrow().clone())
        }
        Object::Buf(b) => {
            weigh(b.borrow().len())?;
            Object::buf(b.borrow().clone())
        }
        Object::Pkg(p) => {
            let p = p.borrow();
            weigh(p.len())?;
            let mut out = Vec::with_capacity(p.len());
            for e in p.iter() {
                out.push(copy_in(e, weight, depth + 1)?);
            }
            Object::Pkg(Rc::new(RefCell::new(out)))
        }
        other => other.clone(),
    })
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
        Object::Buf(b) => Ok(joined(&b.borrow(), b' ', hex2)),
        Object::Uninit => Err(Error::Type("an uninitialized object is used as a source (Table 19.6)")),
        _ => Err(Error::Type("an operand that converts to no string (Table 19.6)")),
    }
}

pub(crate) fn hex2(x: u8) -> Vec<u8> {
    alloc::vec![HEX[usize::from(x >> 4)], HEX[usize::from(x & 0xF)]]
}

pub(crate) fn joined(b: &[u8], sep: u8, each: fn(u8) -> Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, &x) in b.iter().enumerate() {
        if i > 0 {
            out.push(sep);
        }
        out.extend(each(x));
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
