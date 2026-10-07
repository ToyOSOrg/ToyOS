//! Field units over operation regions (§19.6.47, §19.6.63, §19.6.7) and
//! buffer fields over buffers (§19.6.18-23, §19.6.62).
//!
//! A field is read and written in access units: aligned, of the width its
//! access type names, each one [`Host`](crate::Host) access. A unit the field
//! covers in part is completed by the field's update rule. A Lock field holds
//! the Global Lock across the whole access. An IndexField reaches a unit by
//! writing its byte offset to the index field and then accessing the data
//! field; a BankField writes its bank value to the bank field first.

use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;

use crate::exec::Machine;
use crate::name::Seg;
use crate::namespace::NodeId;
use crate::object::{fit, to_buf, to_int, Bytes, Object};
use crate::{Address, Error, MAX_BYTES};

pub(crate) struct Region {
    pub(crate) space: u8,
    pub(crate) base: u64,
    pub(crate) len: u64,
    /// The scope the region was declared in: for PCI_Config, the device it
    /// addresses.
    pub(crate) scope: NodeId,
    pub(crate) pci: Cell<Option<Pci>>,
}

#[derive(Clone, Copy)]
pub(crate) struct Pci {
    segment: u16,
    bus: u8,
    device: u8,
    function: u8,
}

#[derive(Clone)]
pub(crate) enum Kind {
    Region(Rc<Region>),
    Bank { region: Rc<Region>, bank: Rc<Field>, value: u64 },
    Index { index: Rc<Field>, data: Rc<Field> },
}

pub(crate) struct Field {
    pub(crate) kind: Kind,
    pub(crate) bit: u64,
    pub(crate) len: u64,
    /// AccessType (§20.2.5.2): 0 AnyAcc, 1 Byte, 2 Word, 3 DWord, 4 QWord, 5 Buffer.
    pub(crate) access: u8,
    pub(crate) lock: bool,
    /// UpdateRule (§20.2.5.2): 0 Preserve, 1 WriteAsOnes, 2 WriteAsZeros.
    pub(crate) update: u8,
}

pub(crate) struct BufField {
    pub(crate) data: Bytes,
    pub(crate) bit: u64,
    pub(crate) len: u64,
}

/// Bytes enough for `bits`, bounded by what this interpreter holds.
fn bytes_for(bits: u64) -> Result<usize, Error> {
    let n = bits.div_ceil(8);
    usize::try_from(n).ok().filter(|&n| n <= MAX_BYTES).ok_or(Error::Bound("a field larger than this interpreter holds"))
}

fn bit(b: &[u8], i: u64) -> bool {
    b.get((i / 8) as usize).is_some_and(|x| x >> (i % 8) & 1 == 1)
}

fn set_bit(b: &mut [u8], i: u64, on: bool) {
    if let Some(x) = b.get_mut((i / 8) as usize) {
        let m = 1u8 << (i % 8);
        if on { *x |= m } else { *x &= !m }
    }
}


fn width(bytes: u64) -> crate::Access {
    match bytes {
        1 => crate::Access::Byte,
        2 => crate::Access::Word,
        4 => crate::Access::DWord,
        _ => crate::Access::QWord,
    }
}

fn unit_ones(w: u64) -> u64 {
    if w >= 8 { u64::MAX } else { (1u64 << (8 * w)) - 1 }
}

/// The FieldFlags byte (§20.2.5.2): AccessType, LockRule and UpdateRule.
/// Bit 7 is reserved and ignored; a reserved AccessType or UpdateRule is
/// refused where an access would need its meaning.
pub(crate) fn flags(b: u8) -> (u8, bool, u8) {
    (b & 0x0F, b & 0x10 != 0, (b >> 5) & 0x3)
}

impl Machine<'_> {
    /// The bytes of one access unit, by the access type and, for an access
    /// type of AnyAcc, the narrowest naturally aligned unit that holds the
    /// whole field and lies within its region (§19.6.47: "accesses within
    /// the parent object are performed naturally aligned"), else bytes.
    fn unit(&self, f: &Field) -> Result<u64, Error> {
        let region = match &f.kind {
            Kind::Region(r) | Kind::Bank { region: r, .. } => Some(r),
            Kind::Index { .. } => None,
        };
        // Table 19.34: EmbeddedControl, SystemCMOS, GeneralPurposeIO and PCC
        // permit byte access only.
        let bytes_only = matches!(region.map(|r| r.space), Some(0x03 | 0x05 | 0x08 | 0x0A));
        let within = |end: u64| region.is_none_or(|r| end <= r.len);
        let w = match f.access {
            0 if bytes_only => 1,
            0 => [1u64, 2, 4, 8]
                .into_iter()
                .find(|&w| {
                    let unit = f.bit / (8 * w);
                    unit == (f.bit + f.len - 1) / (8 * w) && within((unit + 1) * w)
                })
                .unwrap_or(1),
            1 => 1,
            2 => 2,
            3 => 4,
            4 => 8,
            5 => return Err(Error::Unsupported("BufferAcc, which only the SMBus, IPMI and GenericSerialBus spaces use")),
            _ => return Err(Error::Rule("an access by a reserved AccessType (§20.2.5.2)")),
        };
        if bytes_only && w != 1 {
            return Err(Error::Type("a field wider than a byte in a space Table 19.34 permits ByteAcc alone"));
        }
        Ok(w)
    }

    fn address(&mut self, r: &Region, offset: u64, w: u64) -> Result<Address, Error> {
        let past = || Error::Rule("a field access runs past its operation region (§19.6.47)");
        if offset.checked_add(w).ok_or_else(past)? > r.len {
            return Err(past());
        }
        let at = r.base.checked_add(offset).ok_or_else(past)?;
        match r.space {
            0x00 => Ok(Address::Memory(at)),
            0x01 => {
                let port = u16::try_from(at).ok().filter(|&p| u64::from(p) + w <= 0x1_0000);
                Ok(Address::Io(port.ok_or(Error::Rule("a SystemIO access past port 0xFFFF"))?))
            }
            0x02 => {
                // PCI configuration space is 4096 bytes a function.
                let offset = u16::try_from(at).ok().filter(|&o| u64::from(o) + w <= 0x1000);
                let offset = offset.ok_or(Error::Rule("a PCI_Config access past a function's 4096 bytes"))?;
                let p = self.pci(r)?;
                Ok(Address::PciConfig { segment: p.segment, bus: p.bus, device: p.device, function: p.function, offset })
            }
            0x03 => {
                // §12: the embedded controller's space is 256 bytes.
                let at = u8::try_from(at).ok().filter(|&a| u64::from(a) + w <= 0x100);
                Ok(Address::EmbeddedControl(at.ok_or(Error::Rule("an EmbeddedControl access past its 256 bytes (§12)"))?))
            }
            0x04 => Err(Error::Unsupported("the SMBus address space")),
            0x05 => Err(Error::Unsupported("the SystemCMOS address space")),
            0x06 => Err(Error::Unsupported("the PciBarTarget address space")),
            0x07 => Err(Error::Unsupported("the IPMI address space")),
            0x08 => Err(Error::Unsupported("the GeneralPurposeIO address space")),
            0x09 => Err(Error::Unsupported("the GenericSerialBus address space")),
            0x0A => Err(Error::Unsupported("the PCC address space")),
            0x0B => Err(Error::Unsupported("the PlatformRtMechanism address space")),
            0x7F => Err(Error::Unsupported("the FFixedHW address space")),
            0x0C..=0x7E => Err(Error::Unsupported("a reserved address space (Table 5.182)")),
            _ => Err(Error::Unsupported("an OEM-defined address space")),
        }
    }

    /// The function a PCI_Config region addresses: its device's `_ADR`
    /// (§6.1.1: device in the high word, function in the low), in the
    /// segment group the host bridge's `_SEG` names or 0 without one
    /// (§6.5.6). The host bridge is the nearest scope naming a `_BBN`, which
    /// is the bus directly below it (§6.5.5); each device between it and the
    /// region's is a bridge, whose Secondary Bus Number register is the bus
    /// below it (PCI-to-PCI Bridge Architecture Specification 1.2, §3.2.5.4).
    /// A region declared in the host bridge itself addresses the bridge.
    fn pci(&mut self, r: &Region) -> Result<Pci, Error> {
        if let Some(p) = r.pci.get() {
            return Ok(p);
        }
        let bbn = Seg(*b"_BBN");
        let mut path = Vec::new();
        let mut bridge = r.scope;
        while self.ns.child(bridge, bbn).is_none() {
            self.step()?;
            path.push(bridge);
            let above = self.ns.parent(bridge);
            bridge = above.ok_or(Error::Unsupported("a PCI_Config region below no host bridge, which names a _BBN"))?;
        }
        let bus = self.named_int(bridge, bbn)?.unwrap_or(0);
        let segment = self.named_int(bridge, Seg(*b"_SEG"))?.unwrap_or(0);
        // §6.5.5 and §6.5.6 give the bus in the low 8 bits and the segment
        // group in the low 16, the rest reserved: a value outside them names
        // no bus this access could reach.
        let mut bus = u8::try_from(bus).map_err(|_| Error::Rule("a _BBN above 0xFF (§6.5.5)"))?;
        let segment = u16::try_from(segment).map_err(|_| Error::Rule("a _SEG above 0xFFFF (§6.5.6)"))?;
        for &above in path.iter().skip(1).rev() {
            let b = self.function(above, segment, bus)?;
            let secondary = Address::PciConfig { segment, bus, device: b.device, function: b.function, offset: 0x19 };
            bus = self.host.read(secondary, crate::Access::Byte).map_err(|d| Error::Host(d.0))? as u8;
        }
        let at = self.function(path.first().copied().unwrap_or(bridge), segment, bus)?;
        r.pci.set(Some(at));
        Ok(at)
    }

    /// The function a device's `_ADR` names on `bus` (§6.1.1).
    fn function(&mut self, device: NodeId, segment: u16, bus: u8) -> Result<Pci, Error> {
        let adr = self.named_int(device, Seg(*b"_ADR"))?.ok_or(Error::NotFound(self.ns.path_of(device, Some(Seg(*b"_ADR")))))?;
        let (Ok(device @ 0..=31), Ok(function @ 0..=7)) = (u8::try_from(adr >> 16), u8::try_from(adr & 0xFFFF)) else {
            return Err(Error::Rule("an _ADR that names no single PCI function (§6.1.1)"));
        };
        Ok(Pci { segment, bus, device, function })
    }

    fn unit_read(&mut self, f: &Field, offset: u64, w: u64) -> Result<u64, Error> {
        match &f.kind {
            Kind::Region(r) => {
                let at = self.address(r, offset, w)?;
                self.host.read(at, width(w)).map_err(|d| Error::Host(d.0))
            }
            Kind::Bank { region, bank, value } => {
                self.write_field(bank, Object::Int(*value))?;
                let at = self.address(region, offset, w)?;
                self.host.read(at, width(w)).map_err(|d| Error::Host(d.0))
            }
            Kind::Index { index, data } => {
                self.write_field(index, Object::Int(offset))?;
                let v = self.read_field(data)?;
                to_int(&v, self.w)
            }
        }
    }

    fn unit_write(&mut self, f: &Field, offset: u64, w: u64, v: u64) -> Result<(), Error> {
        match &f.kind {
            Kind::Region(r) => {
                let at = self.address(r, offset, w)?;
                self.host.write(at, width(w), v).map_err(|d| Error::Host(d.0))
            }
            Kind::Bank { region, bank, value } => {
                self.write_field(bank, Object::Int(*value))?;
                let at = self.address(region, offset, w)?;
                self.host.write(at, width(w), v).map_err(|d| Error::Host(d.0))
            }
            Kind::Index { index, data } => {
                self.write_field(index, Object::Int(offset))?;
                self.write_field(data, Object::Int(v))
            }
        }
    }

    pub(crate) fn read_field(&mut self, f: &Field) -> Result<Object, Error> {
        self.enter()?;
        let r = self.locked(f.lock, |m| m.read_units(f));
        self.leave();
        self.value(r?, f.len)
    }

    /// A field's value (§19.6.47): an Integer when it fits one, else a Buffer.
    fn value(&mut self, b: Vec<u8>, bits: u64) -> Result<Object, Error> {
        if bits <= u64::from(self.w.bits) { Ok(Object::Int(self.w.int_of_bytes(&b)?)) } else { self.new_buf(b) }
    }

    fn read_units(&mut self, f: &Field) -> Result<Vec<u8>, Error> {
        let mut out = vec![0u8; bytes_for(f.len)?];
        self.charge(out.len())?;
        let w = self.unit(f)?;
        let span = 8 * w;
        for u in f.bit / span..=(f.bit + f.len - 1) / span {
            self.step()?;
            let v = self.unit_read(f, u * w, w)?;
            let (lo, hi) = (u * span, (u + 1) * span);
            for b in f.bit.max(lo)..(f.bit + f.len).min(hi) {
                set_bit(&mut out, b - f.bit, v >> (b - lo) & 1 == 1);
            }
        }
        Ok(out)
    }

    /// A store to a field unit (Table 19.7): an Integer overwrites the whole
    /// field; a Buffer is written in pieces of the field's size, lower first,
    /// each zero-extended, and an empty one as zeros; a String is written a
    /// character at a time. The pieces are slices of the source, each
    /// written before the next is taken.
    pub(crate) fn write_field(&mut self, f: &Field, v: Object) -> Result<(), Error> {
        let n = bytes_for(f.len)?;
        let (int, held);
        let (source, piece): (&[u8], usize) = match &v {
            Object::Int(x) => {
                int = x.to_le_bytes();
                (&int, int.len())
            }
            Object::Buf(b) => {
                held = b.borrow();
                if held.is_empty() { (&[0], 1) } else { (&held, n) }
            }
            Object::Str(s) => {
                held = s.borrow();
                (&held, 1)
            }
            _ => return Err(Error::Type("a store to a field unit of an object that is not an integer, buffer or string")),
        };
        self.enter()?;
        let r = self.locked(f.lock, |m| source.chunks(piece).try_for_each(|p| m.write_units(f, p)));
        self.leave();
        r
    }

    /// Writes the field from `data`, which is zeros past its end.
    fn write_units(&mut self, f: &Field, data: &[u8]) -> Result<(), Error> {
        let w = self.unit(f)?;
        let span = 8 * w;
        for u in f.bit / span..=(f.bit + f.len - 1) / span {
            self.step()?;
            let (lo, hi) = (u * span, (u + 1) * span);
            let (s, e) = (f.bit.max(lo), (f.bit + f.len).min(hi));
            let mut v = if s == lo && e == hi {
                0
            } else {
                match f.update {
                    0 => self.unit_read(f, u * w, w)?,
                    1 => unit_ones(w),
                    2 => 0,
                    _ => return Err(Error::Rule("a write by a reserved UpdateRule (§20.2.5.2)")),
                }
            };
            for b in s..e {
                let m = 1u64 << (b - lo);
                if bit(data, b - f.bit) { v |= m } else { v &= !m }
            }
            self.unit_write(f, u * w, w, v)?;
        }
        Ok(())
    }

    /// Runs `body` holding the Global Lock where `lock` says (§19.6.47).
    fn locked<T>(&mut self, lock: bool, body: impl FnOnce(&mut Self) -> Result<T, Error>) -> Result<T, Error> {
        if lock {
            self.take_global()?;
        }
        let r = body(self);
        if lock {
            let released = self.drop_global();
            return r.and_then(|v| released.map(|()| v));
        }
        r
    }

    pub(crate) fn read_buf_field(&mut self, f: &BufField) -> Result<Object, Error> {
        // A bit at a time: a byte of work for each.
        self.charge(usize::try_from(f.len).unwrap_or(usize::MAX))?;
        let d = f.data.borrow();
        if f.bit.saturating_add(f.len) > (d.len() as u64).saturating_mul(8) {
            return Err(Error::Rule("a buffer field reaches past its buffer, which shrank since"));
        }
        let mut out = vec![0u8; bytes_for(f.len)?];
        for i in 0..f.len {
            set_bit(&mut out, i, bit(&d, f.bit + i));
        }
        drop(d);
        self.value(out, f.len)
    }

    /// A store to a buffer field (Table 19.7): the source as bytes, truncated
    /// or zero-extended to the field.
    pub(crate) fn write_buf_field(&mut self, f: &BufField, v: Object) -> Result<(), Error> {
        let src = match &v {
            Object::Int(x) => x.to_le_bytes().to_vec(),
            Object::Buf(_) | Object::Str(_) => to_buf(&v, self.w)?,
            _ => return Err(Error::Type("a store to a buffer field of an object that is not an integer, buffer or string")),
        };
        let src = fit(src, bytes_for(f.len)?);
        self.charge(usize::try_from(f.len).unwrap_or(usize::MAX))?;
        let mut d = f.data.bits();
        if f.bit.saturating_add(f.len) > (d.len() as u64).saturating_mul(8) {
            return Err(Error::Rule("a buffer field reaches past its buffer, which shrank since"));
        }
        for i in 0..f.len {
            set_bit(&mut d, f.bit + i, bit(&src, i));
        }
        Ok(())
    }
}
