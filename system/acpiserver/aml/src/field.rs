//! Field units over operation regions (§19.6.47, §19.6.63, §19.6.7) and
//! buffer fields over buffers (§19.6.18-23, §19.6.62).
//!
//! A field of a space addressed by offset is read and written in access
//! units: aligned, of the width its access type names, each one
//! [`Host`](crate::Host) access. A unit the field covers in part is
//! completed by the field's update rule. A field of a space whose protocol is
//! command- or connection-based (§5.5.2.4: SMBus, IPMI, GeneralPurposeIO,
//! GenericSerialBus, PlatformRtMechanism, FFixedHW) is one
//! [`Transfer`](crate::Transfer) whole, with the protocol its AccessAs names
//! and the descriptor its Connection names; a BufferAcc field's value is the
//! buffer the host answers, and a store to it answers the response its
//! buffer became (§5.5.2.4.5.1). A Lock field holds
//! the Global Lock across the whole access. An IndexField reaches a unit by
//! writing its byte offset to the index field and then accessing the data
//! field; a BankField writes its bank value to the bank field first.
//!
//! An access runs other fields' accesses and, for a PCI_Config region,
//! firmware's methods, each of which may access a field again: the buffer a
//! read gathers and the bytes a store writes from are held against the meter
//! before the first unit is accessed. A field that fits an Integer is read,
//! and an Integer stored, without the heap: an interpreter that is full
//! still does both.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::exec::Machine;
use crate::name::Seg;
use crate::namespace::NodeId;
use crate::object::{fit, to_buf, to_int, Bytes, Kept, Object};
use crate::{Address, Error, Transfer, MAX_BYTES};

pub(crate) struct Region {
    pub(crate) space: u8,
    pub(crate) base: u64,
    pub(crate) len: u64,
    /// The scope the region was declared in: for PCI_Config, the device it
    /// addresses, or something inside that device.
    pub(crate) scope: NodeId,
}

struct Pci {
    segment: u16,
    bus: u8,
    device: u8,
    function: u8,
}

#[derive(Clone)]
pub(crate) enum Kind {
    Region(Kept<Region>),
    Bank { region: Kept<Region>, bank: Kept<Field>, value: u64 },
    Index { index: Kept<Field>, data: Kept<Field> },
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
    /// The protocol the AccessAs before it names (Table 5.184), 0 for none,
    /// and the byte count of AttribBytes and the raw protocols.
    pub(crate) attrib: (u8, u8),
    /// The Connection before it (§19.6.15), and the field's bit at which the
    /// units after that connection begin.
    pub(crate) connection: Option<(Connection, u64)>,
}

/// A Connection's resource descriptor (§19.6.15): a buffer the field list
/// holds, or a named object read as a buffer when the field is accessed.
#[derive(Clone)]
pub(crate) enum Connection {
    Bytes(Bytes),
    Named(NodeId),
}

/// A space whose protocol is command- or connection-based (§5.5.2.4): its
/// field is accessed whole, through the host.
pub(crate) fn carried(space: u8) -> bool {
    matches!(space, 0x04 | 0x07 | 0x08 | 0x09 | 0x0B | 0x7F)
}

/// A space whose protocol is BufferAcc's: its field's value is a buffer.
fn buffered(space: u8) -> bool {
    matches!(space, 0x04 | 0x07 | 0x09 | 0x0B | 0x7F)
}

/// The AccessAs attribute an AccessField's or ExtendedAccessField's bytes
/// name (§20.2.5.2): the AccessType byte's bits 7:6 name AttribBytes,
/// AttribRawBytes or AttribRawProcessBytes of the AccessAttrib byte's count,
/// or, as 0, the AccessAttrib byte itself; an ExtendedAccessField names its
/// attribute and its count.
pub(crate) fn attrib(access_type: u8, access_attrib: u8, extended: Option<u8>) -> (u8, u8) {
    match (extended, access_type >> 6) {
        (Some(length), _) => (access_attrib, length),
        (None, 0) => (access_attrib, 0),
        (None, 1) => (0x0B, access_attrib),
        (None, 2) => (0x0E, access_attrib),
        (None, _) => (0x0F, access_attrib),
    }
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

/// The region a field of a carried space is in, or `None` for a field
/// accessed by unit; a BankField or IndexField reaches a carried space by no
/// unit, and is refused.
fn carried_region(f: &Field) -> Result<Option<Kept<Region>>, Error> {
    match &f.kind {
        Kind::Region(r) if carried(r.space) => Ok(Some(r.clone())),
        Kind::Bank { region, .. } if carried(region.space) => {
            Err(Error::Rule("a BankField of a space whose protocol is command- or connection-based (§5.5.2.4)"))
        }
        _ => Ok(None),
    }
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
        // Table 19.34: EmbeddedControl, SystemCMOS and PCC permit byte access
        // only; GeneralPurposeIO is carried whole.
        let bytes_only = matches!(region.map(|r| r.space), Some(0x03 | 0x05 | 0x0A));
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
            5 => return Err(Error::Rule("BufferAcc in a space addressed by offset, which takes no buffer (§19.6.47)")),
            _ => return Err(Error::Rule("an access by a reserved AccessType (§20.2.5.2)")),
        };
        if bytes_only && w != 1 {
            return Err(Error::Type("a field wider than a byte in a space Table 19.34 permits ByteAcc alone"));
        }
        Ok(w)
    }

    fn address(&mut self, r: &Region, offset: u64, w: u64) -> Result<Address, Error> {
        let past = || Error::Rule("a field access runs past its operation region (§19.6.47)");
        // A PciBarTarget region's length is its BAR's (§5.5.2.4.4.1), which
        // the host reads.
        if r.space != 0x06 && offset.checked_add(w).ok_or_else(past)? > r.len {
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
            0x05 => {
                // The RTC/CMOS device's bytes, 256 at the most an extension names.
                let at = u8::try_from(at).ok().filter(|&a| u64::from(a) + w <= 0x100);
                Ok(Address::SystemCmos(at.ok_or(Error::Rule("a SystemCMOS access past the CMOS's 256 bytes (§5.5.2.4.3)"))?))
            }
            0x06 => {
                // §5.5.2.4.4.1: the region's offset is a BAR's in the
                // function's configuration space, 0x10 to 0x24.
                let bar = u8::try_from(r.base).ok().filter(|b| (0x10..=0x24).contains(b) && b % 4 == 0);
                let bar = bar.ok_or(Error::Rule("a PciBarTarget region whose offset is no BAR's (§5.5.2.4.4.1)"))?;
                let p = self.pci(r)?;
                Ok(Address::PciBarTarget { segment: p.segment, bus: p.bus, device: p.device, function: p.function, bar, offset })
            }
            0x0A => {
                // §5.5.2.4.8.2: the region's offset is the subspace's ID.
                let subspace = u8::try_from(r.base).map_err(|_| Error::Rule("a PCC region whose subspace ID is past 255 (§5.5.2.4.8.2)"))?;
                let offset = u32::try_from(offset).map_err(|_| past())?;
                Ok(Address::Pcc { subspace, offset })
            }
            0x80..=0xFF => Ok(Address::Oem { space: r.space, offset: at }),
            0x0C..=0x7E => Err(Error::Rule("a region of a reserved address space (Table 5.182)")),
            _ => Err(Error::Rule("a field of a space whose protocol is the host's, accessed by unit")),
        }
    }

    /// The function a PCI_Config region addresses: that of the nearest device
    /// its scope is or lies in, by the device's `_ADR` (§6.1.1: device in
    /// the high word, function in the low), in the segment group the host
    /// bridge's `_SEG` names or 0 without one (§6.5.6). The host bridge is
    /// the nearest device naming a `_BBN`, which is the bus directly below it
    /// (§6.5.5), or else whose `_HID` or one of whose `_CID`s (§6.1.5,
    /// §6.1.2) names a PCI or PCI Express host bridge, PNP0A03 or PNP0A08,
    /// on bus 0, the only root bus a machine that names no `_BBN` has; each
    /// device between it and the region's is a PCI-to-PCI
    /// bridge by its Header Type register, whose Secondary Bus Number
    /// register is the bus below it (PCI-to-PCI Bridge Architecture
    /// Specification 1.2, §3.2.5.4). A region declared in the host bridge
    /// itself addresses the bridge.
    ///
    /// §6.5.4 holds a PCI_Config region accessible always only on a root bus
    /// naming a `_BBN`; one on a root bus that names none once its `_REG(2,
    /// 1)` has run, which is the caller's to run; and one below a bridge once
    /// "the bridge controller has been programmed with a bus number": a
    /// region below anything else is refused, where any bus chosen for it
    /// would be another device's.
    ///
    /// Every access asks again, firmware's methods and the bridges both:
    /// nothing is kept that a bridge renumbered since would make stale. A
    /// bridge's secondary bus is above the bus it is on and a bus number is
    /// 8 bits, so a region below more than 255 bridges is on no bus, and is
    /// refused before any is asked.
    fn pci(&mut self, r: &Region) -> Result<Pci, Error> {
        let bbn = Seg(*b"_BBN");
        let mut below = Vec::new();
        let mut host = r.scope;
        loop {
            self.step()?;
            if matches!(self.ns.object(host), Some(Object::Device)) {
                if self.ns.child(host, bbn).is_some() || self.host_bridge(host)? {
                    break;
                }
                if below.len() > usize::from(u8::MAX) {
                    return Err(Error::Rule("a PCI_Config region below more bridges than there are buses for them"));
                }
                below.push(host);
            }
            let above = self.ns.parent(host);
            host = above.ok_or(Error::Rule("a PCI_Config region below no host bridge, which names a _BBN or is PNP0A03 or PNP0A08 by its _HID or _CID"))?;
        }
        let bus = self.named_int(host, bbn)?.unwrap_or(0);
        let segment = self.named_int(host, Seg(*b"_SEG"))?.unwrap_or(0);
        // §6.5.5 and §6.5.6 give the bus in the low 8 bits and the segment
        // group in the low 16, the rest reserved: a value outside them names
        // no bus this access could reach.
        let mut bus = u8::try_from(bus).map_err(|_| Error::Rule("a _BBN above 0xFF (§6.5.5)"))?;
        let segment = u16::try_from(segment).map_err(|_| Error::Rule("a _SEG above 0xFFFF (§6.5.6)"))?;
        let Some((&device, bridges)) = below.split_first() else { return self.function(host, segment, bus) };
        for &bridge in bridges.iter().rev() {
            let b = self.function(bridge, segment, bus)?;
            let register = |m: &mut Self, offset| {
                let at = Address::PciConfig { segment, bus, device: b.device, function: b.function, offset };
                m.host.read(at, crate::Access::Byte).map(|v| v as u8).map_err(|d| Error::Host(d.0))
            };
            // Offset 0x19 is a Secondary Bus Number only in header layout 1,
            // the low seven bits of the Header Type: a function that is
            // absent answers all ones, and any other layout a byte of
            // something else.
            let header_type = register(self, 0x0E)?;
            let refused = |secondary| Error::Bridge { segment, bus, device: b.device, function: b.function, header_type, secondary };
            if header_type & 0x7F != 0x01 {
                return Err(refused(None));
            }
            let answered = register(self, 0x19)?;
            // §6.5.4: the region is ready once its bridge has a bus number.
            // The register resets to 0, and a bridge's secondary bus is
            // above the bus the bridge is on: any other answer would address
            // a device that is not below this bridge.
            if answered <= bus {
                return Err(refused(Some(answered)));
            }
            bus = answered;
        }
        self.function(device, segment, bus)
    }

    /// Whether a device is a PCI host bridge by its `_HID` or a `_CID`: an
    /// EISA ID's integer or a string, a `_CID` one or a package of them.
    fn host_bridge(&mut self, device: NodeId) -> Result<bool, Error> {
        // EisaId ("PNP0A03") and EisaId ("PNP0A08") as AML stores them.
        const IDS: [u64; 2] = [0x030A_D041, 0x080A_D041];
        const TEXT: [&[u8]; 2] = [b"PNP0A03", b"PNP0A08"];
        for seg in [Seg(*b"_HID"), Seg(*b"_CID")] {
            let ids = match self.named_object(device, seg)? {
                None => continue,
                Some(Object::Pkg(p)) => p.borrow().clone(),
                Some(id) => vec![id],
            };
            for id in ids {
                let hit = match self.resolve_lazy(id)? {
                    Object::Int(id) => IDS.contains(&id),
                    Object::Str(text) => TEXT.contains(&text.borrow().as_slice()),
                    _ => false,
                };
                if hit {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// The function a device's `_ADR` names on `bus` (§6.1.1).
    fn function(&mut self, device: NodeId, segment: u16, bus: u8) -> Result<Pci, Error> {
        let adr = Seg(*b"_ADR");
        let Some(adr) = self.named_int(device, adr)? else { return Err(Error::NotFound(self.path_of(device, Some(adr))?)) };
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
        let r = self.locked(f.lock, |m| match carried_region(f)? {
            Some(region) => m.transfer(f, &region, None),
            None => m.read_units(f),
        });
        self.leave();
        r
    }

    /// One [`Transfer`] of a carried field whole: a read where `write` is
    /// `None`, else a write of its bytes. A BufferAcc field's value is the
    /// buffer the host answered; a GeneralPurposeIO field's, its pins as an
    /// Integer or a Buffer as wide as it is (§19.6.47).
    fn transfer(&mut self, f: &Field, region: &Region, write: Option<&[u8]>) -> Result<Object, Error> {
        let connection = match &f.connection {
            None => None,
            Some((Connection::Bytes(b), at)) => Some((b.clone(), *at)),
            Some((Connection::Named(id), at)) => match self.ns.object(*id).cloned() {
                Some(Object::Buf(b)) => Some((b, *at)),
                Some(Object::BufField(b)) => match self.read_buf_field(&b)? {
                    Object::Buf(b) => Some((b, *at)),
                    _ => return Err(Error::Type("a Connection whose descriptor is no Buffer (§19.6.15)")),
                },
                Some(_) => return Err(Error::Type("a Connection whose descriptor is no Buffer (§19.6.15)")),
                None => return Err(Error::NotFound(String::from("a Connection's descriptor its method's exit destroyed"))),
            },
        };
        let held = connection.as_ref().map(|(b, at)| (b.borrow(), *at));
        let at = Transfer {
            space: region.space,
            region: region.base,
            bit: f.bit,
            bits: f.len,
            attrib: f.attrib.0,
            length: f.attrib.1,
            connection: held.as_ref().map(|(b, at)| (b.as_slice(), *at)),
        };
        let answer = self.host.transfer(&at, write).map_err(|d| Error::Host(d.0))?;
        drop(held);
        crate::object::bounded(answer.len())?;
        if buffered(region.space) { self.new_buf(answer) } else { self.value(fit(answer, bytes_for(f.len)?), f.len) }
    }

    /// Whether a field's value is a Buffer (§19.6.47): an Integer when it
    /// fits one, else a Buffer.
    fn wide(&self, bits: u64) -> bool {
        bits > u64::from(self.w.bits)
    }

    fn value(&mut self, b: Vec<u8>, bits: u64) -> Result<Object, Error> {
        if self.wide(bits) { self.new_buf(b) } else { Ok(Object::Int(self.w.int_of_bytes(&b)?)) }
    }

    /// A field's value, gathered unit by unit: an Integer in a word, a
    /// Buffer in bytes held before the first unit is read.
    fn read_units(&mut self, f: &Field) -> Result<Object, Error> {
        let n = bytes_for(f.len)?;
        let buf = if self.wide(f.len) {
            Some(self.bytes(vec![0u8; n])?)
        } else {
            self.charge(n)?;
            None
        };
        let mut int = 0u64;
        let w = self.unit(f)?;
        let span = 8 * w;
        for u in f.bit / span..=(f.bit + f.len - 1) / span {
            self.step()?;
            let v = self.unit_read(f, u * w, w)?;
            let (lo, hi) = (u * span, (u + 1) * span);
            let mut bits = buf.as_ref().map(|b| b.bits());
            for b in f.bit.max(lo)..(f.bit + f.len).min(hi) {
                let on = v >> (b - lo) & 1;
                match &mut bits {
                    Some(bits) => set_bit(bits, b - f.bit, on == 1),
                    None => int |= on << (b - f.bit),
                }
            }
        }
        Ok(buf.map_or(Object::Int(int), Object::Buf))
    }

    /// A store to a field unit (Table 19.7): an Integer overwrites the whole
    /// field; a Buffer is written in pieces of the field's size, lower first,
    /// each zero-extended, and an empty one as zeros; a String is written a
    /// character at a time. The pieces are slices of the store's own copy of
    /// the source, each written before the next is taken: a write runs
    /// firmware's methods, which may store to the source, and the store is of
    /// what the source held when it began.
    pub(crate) fn write_field(&mut self, f: &Field, v: Object) -> Result<(), Error> {
        self.write_field_answering(f, v).map(drop)
    }

    /// A store to a field, answering the response a carried BufferAcc
    /// field's write became, and `None` for any other field.
    pub(crate) fn write_field_answering(&mut self, f: &Field, v: Object) -> Result<Option<Object>, Error> {
        if let Some(region) = carried_region(f)? {
            let bytes = match &v {
                Object::Int(x) => fit(x.to_le_bytes().to_vec(), bytes_for(f.len)?),
                Object::Buf(_) | Object::Str(_) => to_buf(&v, self.w)?,
                _ => return Err(Error::Type("a store to a field unit of an object that is not an integer, buffer or string")),
            };
            let copy = self.bytes(bytes)?;
            self.enter()?;
            let r = self.locked(f.lock, |m| m.transfer(f, &region, Some(&copy.borrow())));
            self.leave();
            return r.map(|answer| buffered(region.space).then_some(answer));
        }
        self.write_by_units(f, v).map(|()| None)
    }

    fn write_by_units(&mut self, f: &Field, v: Object) -> Result<(), Error> {
        let n = bytes_for(f.len)?;
        let (int, copy, held);
        let (source, piece): (&[u8], usize) = match &v {
            Object::Int(x) => {
                int = x.to_le_bytes();
                (&int, int.len())
            }
            Object::Buf(b) | Object::Str(b) => {
                let bytes = b.borrow().clone();
                copy = self.bytes(bytes)?;
                held = copy.borrow();
                match &v {
                    Object::Str(_) => (&held, 1),
                    _ if held.is_empty() => (&[0], 1),
                    _ => (&held, n),
                }
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
            self.take_global(None)?;
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
        // The source is read whole, and the field written a bit at a time.
        self.charge(src.len().saturating_add(usize::try_from(f.len).unwrap_or(usize::MAX)))?;
        let src = fit(src, bytes_for(f.len)?);
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
