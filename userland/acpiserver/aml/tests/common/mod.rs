//! AML assembled by hand from the encodings of §20.2, and a host that
//! records what the interpreter asks of it.

#![allow(dead_code)]

use std::collections::BTreeMap;

use toyos_acpi::{Phys, Table};
use toyos_aml::{Access, Address, Denied, Error, Host, Interpreter, Value};

/// PkgLength (§20.2.4) of `n` bytes that follow it: the length counts its
/// own encoding.
pub fn pkg(body: &[u8]) -> Vec<u8> {
    let n = body.len();
    let mut out = if n < 0x3F {
        vec![(n + 1) as u8]
    } else {
        let follow = if n + 2 <= 0xFFF { 1 } else if n + 3 <= 0xF_FFFF { 2 } else { 3 };
        let v = n + 1 + follow;
        let mut b = vec![((follow as u8) << 6) | (v & 0xF) as u8];
        for i in 0..follow {
            b.push((v >> (4 + 8 * i)) as u8);
        }
        b
    };
    out.extend_from_slice(body);
    out
}

/// A NameString (§20.2.2) from text: `\`, `^`, and `.`-separated segments
/// padded with `_`.
pub fn name(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = text;
    if let Some(r) = rest.strip_prefix('\\') {
        out.push(b'\\');
        rest = r;
    }
    while let Some(r) = rest.strip_prefix('^') {
        out.push(b'^');
        rest = r;
    }
    let segs: Vec<[u8; 4]> = if rest.is_empty() {
        Vec::new()
    } else {
        rest.split('.')
            .map(|s| {
                let mut seg = [b'_'; 4];
                seg[..s.len()].copy_from_slice(s.as_bytes());
                seg
            })
            .collect()
    };
    match segs.len() {
        0 => out.push(0x00),
        1 => {}
        2 => out.push(0x2E),
        n => out.extend([0x2F, n as u8]),
    }
    for s in segs {
        out.extend(s);
    }
    out
}

pub fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

pub fn int(v: u64) -> Vec<u8> {
    match v {
        0 => vec![0x00],
        1 => vec![0x01],
        2..=0xFF => vec![0x0A, v as u8],
        0x100..=0xFFFF => cat(&[&[0x0B], &(v as u16).to_le_bytes()]),
        0x1_0000..=0xFFFF_FFFF => cat(&[&[0x0C], &(v as u32).to_le_bytes()]),
        _ => cat(&[&[0x0E], &v.to_le_bytes()]),
    }
}

pub fn ones() -> Vec<u8> {
    vec![0xFF]
}

pub fn string(s: &str) -> Vec<u8> {
    cat(&[&[0x0D], s.as_bytes(), &[0]])
}

pub fn buffer(size: &[u8], init: &[u8]) -> Vec<u8> {
    cat(&[&[0x11], &pkg(&cat(&[size, init]))])
}

pub fn package(elems: &[Vec<u8>]) -> Vec<u8> {
    let mut body = vec![elems.len() as u8];
    for e in elems {
        body.extend(e);
    }
    cat(&[&[0x12], &pkg(&body)])
}

pub fn var_package(count: &[u8], elems: &[Vec<u8>]) -> Vec<u8> {
    let mut body = count.to_vec();
    for e in elems {
        body.extend(e);
    }
    cat(&[&[0x13], &pkg(&body)])
}

pub fn local(i: u8) -> Vec<u8> {
    vec![0x60 + i]
}

pub fn arg(i: u8) -> Vec<u8> {
    vec![0x68 + i]
}

pub fn debug() -> Vec<u8> {
    vec![0x5B, 0x31]
}

pub fn def_name(n: &str, v: &[u8]) -> Vec<u8> {
    cat(&[&[0x08], &name(n), v])
}

pub fn scope(n: &str, body: &[u8]) -> Vec<u8> {
    cat(&[&[0x10], &pkg(&cat(&[&name(n), body]))])
}

pub fn device(n: &str, body: &[u8]) -> Vec<u8> {
    cat(&[&[0x5B, 0x82], &pkg(&cat(&[&name(n), body]))])
}

pub fn thermal_zone(n: &str, body: &[u8]) -> Vec<u8> {
    cat(&[&[0x5B, 0x85], &pkg(&cat(&[&name(n), body]))])
}

pub fn power_resource(n: &str, body: &[u8]) -> Vec<u8> {
    cat(&[&[0x5B, 0x84], &pkg(&cat(&[&name(n), &[0x00, 0x00, 0x00], body]))])
}

/// MethodFlags (§20.2.5.2): ArgCount, SerializeFlag, SyncLevel.
pub fn method_flags(n: &str, flags: u8, body: &[u8]) -> Vec<u8> {
    cat(&[&[0x14], &pkg(&cat(&[&name(n), &[flags], body]))])
}

pub fn method(n: &str, args: u8, body: &[u8]) -> Vec<u8> {
    method_flags(n, args, body)
}

pub fn ret(v: &[u8]) -> Vec<u8> {
    cat(&[&[0xA4], v])
}

pub fn store(v: &[u8], t: &[u8]) -> Vec<u8> {
    cat(&[&[0x70], v, t])
}

pub fn copy_object(v: &[u8], t: &[u8]) -> Vec<u8> {
    cat(&[&[0x9D], v, t])
}

/// One of the `Operand Operand Target` opcodes (§20.2.5.4).
pub fn op2(op: u8, a: &[u8], b: &[u8], t: &[u8]) -> Vec<u8> {
    cat(&[&[op], a, b, t])
}

pub fn op1(op: u8, a: &[u8], t: &[u8]) -> Vec<u8> {
    cat(&[&[op], a, t])
}

pub fn add(a: &[u8], b: &[u8], t: &[u8]) -> Vec<u8> {
    op2(0x72, a, b, t)
}

pub fn lequal(a: &[u8], b: &[u8]) -> Vec<u8> {
    cat(&[&[0x93], a, b])
}

pub fn lless(a: &[u8], b: &[u8]) -> Vec<u8> {
    cat(&[&[0x95], a, b])
}

pub fn lgreater(a: &[u8], b: &[u8]) -> Vec<u8> {
    cat(&[&[0x94], a, b])
}

pub fn lnot(a: &[u8]) -> Vec<u8> {
    cat(&[&[0x92], a])
}

pub fn if_(pred: &[u8], body: &[u8]) -> Vec<u8> {
    cat(&[&[0xA0], &pkg(&cat(&[pred, body]))])
}

pub fn else_(body: &[u8]) -> Vec<u8> {
    cat(&[&[0xA1], &pkg(body)])
}

pub fn while_(pred: &[u8], body: &[u8]) -> Vec<u8> {
    cat(&[&[0xA2], &pkg(&cat(&[pred, body]))])
}

pub fn increment(t: &[u8]) -> Vec<u8> {
    cat(&[&[0x75], t])
}

pub fn index(src: &[u8], i: &[u8], t: &[u8]) -> Vec<u8> {
    cat(&[&[0x88], src, i, t])
}

pub fn deref(r: &[u8]) -> Vec<u8> {
    cat(&[&[0x83], r])
}

pub fn ref_of(n: &[u8]) -> Vec<u8> {
    cat(&[&[0x71], n])
}

pub fn size_of(n: &[u8]) -> Vec<u8> {
    cat(&[&[0x87], n])
}

pub fn object_type(n: &[u8]) -> Vec<u8> {
    cat(&[&[0x8E], n])
}

pub fn op_region(n: &str, space: u8, offset: &[u8], len: &[u8]) -> Vec<u8> {
    cat(&[&[0x5B, 0x80], &name(n), &[space], offset, len])
}

/// A field list entry: a named unit of `bits`.
pub fn unit(n: &str, bits: usize) -> Vec<u8> {
    let mut seg = [b'_'; 4];
    seg[..n.len()].copy_from_slice(n.as_bytes());
    cat(&[&seg, &pkg_value(bits)])
}

/// A ReservedField (§20.2.5.2) skipping `bits`.
pub fn skip(bits: usize) -> Vec<u8> {
    cat(&[&[0x00], &pkg_value(bits)])
}

/// The PkgLength encoding of a bare value, as a field list uses it.
pub fn pkg_value(v: usize) -> Vec<u8> {
    if v <= 0x3F {
        vec![v as u8]
    } else {
        let follow = if v <= 0xFFF { 1 } else if v <= 0xF_FFFF { 2 } else { 3 };
        let mut b = vec![((follow as u8) << 6) | (v & 0xF) as u8];
        for i in 0..follow {
            b.push((v >> (4 + 8 * i)) as u8);
        }
        b
    }
}

pub fn field(region: &str, flags: u8, units: &[Vec<u8>]) -> Vec<u8> {
    cat(&[&[0x5B, 0x81], &pkg(&cat(&[&name(region), &[flags], &units.concat()]))])
}

pub fn index_field(index: &str, data: &str, flags: u8, units: &[Vec<u8>]) -> Vec<u8> {
    cat(&[&[0x5B, 0x86], &pkg(&cat(&[&name(index), &name(data), &[flags], &units.concat()]))])
}

pub fn bank_field(region: &str, bank: &str, value: &[u8], flags: u8, units: &[Vec<u8>]) -> Vec<u8> {
    cat(&[&[0x5B, 0x87], &pkg(&cat(&[&name(region), &name(bank), value, &[flags], &units.concat()]))])
}

/// A definition block (§20.2.1, §5.2.6) with its checksum.
pub fn table(signature: &[u8; 4], revision: u8, body: &[u8]) -> Vec<u8> {
    let len = (36 + body.len()) as u32;
    let mut t = Vec::new();
    t.extend(signature);
    t.extend(len.to_le_bytes());
    t.push(revision);
    t.push(0);
    t.extend(b"TOYOS ");
    t.extend(b"TESTTABL");
    t.extend(1u32.to_le_bytes());
    t.extend(b"TOYO");
    t.extend(1u32.to_le_bytes());
    t.extend(body);
    let sum = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    t[9] = 0u8.wrapping_sub(sum);
    t
}

pub fn dsdt(body: &[u8]) -> Vec<u8> {
    table(b"DSDT", 2, body)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Read(Address, Access),
    Write(Address, Access, u64),
    Sleep(u64),
    Stall(u64),
    Notify(String, u64),
    GlobalLock(bool),
}

/// A machine of bytes: every address space a sparse map, every request
/// recorded.
#[derive(Default)]
pub struct Machine {
    pub bytes: BTreeMap<(u8, u64), u8>,
    pub log: Vec<Event>,
    pub refuse: bool,
}

fn key(a: Address) -> (u8, u64) {
    match a {
        Address::Memory(x) => (0, x),
        Address::Io(x) => (1, u64::from(x)),
        Address::PciConfig { segment, bus, device, function, offset } => (
            2,
            (u64::from(segment) << 32)
                | (u64::from(bus) << 24)
                | (u64::from(device) << 19)
                | (u64::from(function) << 16)
                | u64::from(offset),
        ),
        Address::EmbeddedControl(x) => (3, u64::from(x)),
    }
}

fn bytes(w: Access) -> u64 {
    match w {
        Access::Byte => 1,
        Access::Word => 2,
        Access::DWord => 4,
        Access::QWord => 8,
    }
}

impl Machine {
    pub fn poke(&mut self, a: Address, v: &[u8]) {
        let (s, x) = key(a);
        for (i, b) in v.iter().enumerate() {
            self.bytes.insert((s, x + i as u64), *b);
        }
    }

    pub fn peek(&self, a: Address, n: usize) -> Vec<u8> {
        let (s, x) = key(a);
        (0..n).map(|i| *self.bytes.get(&(s, x + i as u64)).unwrap_or(&0)).collect()
    }

    pub fn accesses(&self) -> Vec<Event> {
        self.log.iter().filter(|e| matches!(e, Event::Read(..) | Event::Write(..))).cloned().collect()
    }
}

impl Host for Machine {
    fn read(&mut self, at: Address, width: Access) -> Result<u64, Denied> {
        self.log.push(Event::Read(at, width));
        if self.refuse {
            return Err(Denied("refused".into()));
        }
        let (s, x) = key(at);
        Ok((0..bytes(width)).fold(0, |v, i| v | u64::from(*self.bytes.get(&(s, x + i)).unwrap_or(&0)) << (8 * i)))
    }

    fn write(&mut self, at: Address, width: Access, value: u64) -> Result<(), Denied> {
        self.log.push(Event::Write(at, width, value));
        if self.refuse {
            return Err(Denied("refused".into()));
        }
        let (s, x) = key(at);
        for i in 0..bytes(width) {
            self.bytes.insert((s, x + i), (value >> (8 * i)) as u8);
        }
        Ok(())
    }

    fn sleep(&mut self, ms: u64) {
        self.log.push(Event::Sleep(ms));
    }

    fn stall(&mut self, us: u64) {
        self.log.push(Event::Stall(us));
    }

    fn timer(&mut self) -> u64 {
        0
    }

    fn notify(&mut self, object: &str, value: u64) {
        self.log.push(Event::Notify(object.into(), value));
    }

    fn global_lock(&mut self, take: bool) -> Result<(), Denied> {
        self.log.push(Event::GlobalLock(take));
        Ok(())
    }
}

/// A table's bytes as physical memory at address 0, for
/// [`toyos_acpi::Table::open`].
#[derive(Clone, Copy)]
pub struct Image<'a>(pub &'a [u8]);

impl Phys for Image<'_> {
    fn readable(self, phys: u64, len: usize) -> bool {
        usize::try_from(phys).ok().and_then(|p| p.checked_add(len)).is_some_and(|e| e <= self.0.len())
    }

    fn byte(self, phys: u64) -> u8 {
        self.0[phys as usize]
    }
}

/// Loading from bytes, through the `Table::open` the server reaches a table by.
pub trait LoadBytes {
    fn load_bytes(&mut self, m: &mut Machine, t: &[u8]) -> Result<(), Error>;
}

impl LoadBytes for Interpreter {
    fn load_bytes(&mut self, m: &mut Machine, t: &[u8]) -> Result<(), Error> {
        let signature: [u8; 4] = t.get(..4).and_then(|s| s.try_into().ok()).expect("a test table has a signature");
        let table = Table::open(Image(t), 0, &signature, 0).expect("a test table opens");
        self.load(m, &table)
    }
}

/// An interpreter with one DSDT of `body` loaded.
pub fn loaded(body: &[u8]) -> (Interpreter, Machine) {
    let mut m = Machine::default();
    let mut i = Interpreter::new();
    i.load_bytes(&mut m, &dsdt(body)).expect("the DSDT loads");
    (i, m)
}

/// The value of `\RES` after running method `\MAIN`, whose body is given.
pub fn run(body: &[u8]) -> Value {
    let (mut i, mut m) = loaded(&cat(&[&def_name("RES", &int(0)), &method("MAIN", 0, body)]));
    i.evaluate(&mut m, "\\MAIN", &[]).expect("MAIN runs");
    i.evaluate(&mut m, "\\RES", &[]).expect("RES reads")
}

/// What method `\MAIN`, whose body is given, returns.
pub fn returns(body: &[u8]) -> Result<Value, toyos_aml::Error> {
    let (mut i, mut m) = loaded(&method("MAIN", 0, body));
    i.evaluate(&mut m, "\\MAIN", &[])
}

pub fn s(text: &str) -> Value {
    Value::String(text.as_bytes().to_vec())
}
