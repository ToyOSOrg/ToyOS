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
use crate::{Address, Error, Width, MAX_BYTES};

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

/// A field's value (§19.6.47): an Integer when it fits one, else a Buffer.
fn value(b: Vec<u8>, bits: u64, w: Width) -> Result<Object, Error> {
    if bits <= u64::from(w.bits) { Ok(Object::Int(w.int_of_bytes(&b)?)) } else { Ok(Object::buf(b)) }
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

/// The FieldFlags byte (§20.2.5.2), refused where it sets what is reserved.
pub(crate) fn flags(b: u8) -> Result<(u8, bool, u8), &'static str> {
    let access = b & 0x0F;
    let update = (b >> 5) & 0x3;
    if access > 5 {
        return Err("a field's AccessType is reserved (§20.2.5.2)");
    }
    if update == 3 {
        return Err("a field's UpdateRule is reserved (§20.2.5.2)");
    }
    if b & 0x80 != 0 {
        return Err("a field's FieldFlags sets reserved bit 7 (§20.2.5.2)");
    }
    Ok((access, b & 0x10 != 0, update))
}

impl Machine<'_> {
    /// The bytes of one access unit, by the access type and, for an access
    /// type of AnyAcc, the narrowest naturally aligned unit holding the whole
    /// field (§19.6.47: "accesses within the parent object are performed
    /// naturally aligned"), else bytes.
    fn unit(&self, f: &Field) -> Result<u64, Error> {
        let space = match &f.kind {
            Kind::Region(r) | Kind::Bank { region: r, .. } => Some(r.space),
            Kind::Index { .. } => None,
        };
        // Table 19.34: EmbeddedControl, SystemCMOS, GeneralPurposeIO and PCC
        // permit byte access only.
        let bytes_only = matches!(space, Some(0x03 | 0x05 | 0x08 | 0x0A));
        let w = match f.access {
            0 if bytes_only => 1,
            0 => [1u64, 2, 4, 8]
                .into_iter()
                .find(|&w| f.bit / (8 * w) == (f.bit + f.len - 1) / (8 * w))
                .unwrap_or(1),
            1 => 1,
            2 => 2,
            3 => 4,
            4 => 8,
            _ => return Err(Error::Unsupported("BufferAcc, which only the SMBus, IPMI and GenericSerialBus spaces use")),
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
            0x01 => Ok(Address::Io(at)),
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
            _ => Err(Error::Unsupported("an OEM-defined address space")),
        }
    }

    /// The function a PCI_Config region addresses: its device's `_ADR`
    /// (§6.1.1: device in the high word, function in the low), on the bus a
    /// host bridge's `_BBN` names (§6.5.5), in the segment group its `_SEG`
    /// names or 0 without one (§6.5.6). The region is declared in the host
    /// bridge itself or in a device directly below it.
    fn pci(&mut self, r: &Region) -> Result<Pci, Error> {
        if let Some(p) = r.pci.get() {
            return Ok(p);
        }
        let device = r.scope;
        let bbn = Seg(*b"_BBN");
        let bridge = if self.ns.child(device, bbn).is_some() {
            device
        } else {
            match self.ns.parent(device) {
                Some(p) if self.ns.child(p, bbn).is_some() => p,
                _ => return Err(Error::Unsupported("a PCI_Config region not on a host bridge's bus, which names a _BBN")),
            }
        };
        let adr = self.named_int(device, Seg(*b"_ADR"))?.ok_or(Error::NotFound(self.ns.path_of(device, Some(Seg(*b"_ADR")))))?;
        let bus = self.named_int(bridge, bbn)?.unwrap_or(0);
        let segment = self.named_int(bridge, Seg(*b"_SEG"))?.unwrap_or(0);
        let (dev, fun) = (adr >> 16 & 0xFFFF, adr & 0xFFFF);
        if dev > 31 || fun > 7 {
            return Err(Error::Rule("an _ADR that names no single PCI function (§6.1.1)"));
        }
        let p = Pci { segment: segment as u16, bus: bus as u8, device: dev as u8, function: fun as u8 };
        r.pci.set(Some(p));
        Ok(p)
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
        value(r?, f.len, self.w)
    }

    fn read_units(&mut self, f: &Field) -> Result<Vec<u8>, Error> {
        let mut out = vec![0u8; bytes_for(f.len)?];
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
    /// each zero-extended; a String is written a character at a time.
    pub(crate) fn write_field(&mut self, f: &Field, v: Object) -> Result<(), Error> {
        let n = bytes_for(f.len)?;
        let pieces: Vec<Vec<u8>> = match &v {
            Object::Int(x) => vec![fit(x.to_le_bytes().to_vec(), n)],
            Object::Buf(b) if b.borrow().is_empty() => vec![vec![0; n]],
            Object::Buf(b) => b.borrow().chunks(n).map(|c| fit(c.to_vec(), n)).collect(),
            Object::Str(s) => s.borrow().iter().map(|&c| fit(vec![c], n)).collect(),
            _ => return Err(Error::Type("a store to a field unit of an object that is not an integer, buffer or string")),
        };
        self.enter()?;
        let r = self.locked(f.lock, |m| pieces.iter().try_for_each(|p| m.write_units(f, p)));
        self.leave();
        r
    }

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
                    _ => 0,
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
        let d = f.data.borrow();
        if f.bit.saturating_add(f.len) > (d.len() as u64).saturating_mul(8) {
            return Err(Error::Rule("a buffer field reaches past its buffer, which shrank since"));
        }
        let mut out = vec![0u8; bytes_for(f.len)?];
        for i in 0..f.len {
            set_bit(&mut out, i, bit(&d, f.bit + i));
        }
        drop(d);
        value(out, f.len, self.w)
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
        let mut d = f.data.borrow_mut();
        if f.bit.saturating_add(f.len) > (d.len() as u64).saturating_mul(8) {
            return Err(Error::Rule("a buffer field reaches past its buffer, which shrank since"));
        }
        for i in 0..f.len {
            set_bit(&mut d, f.bit + i, bit(&src, i));
        }
        Ok(())
    }
}
