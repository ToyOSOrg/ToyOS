#![allow(dead_code)]

//! A machine's physical memory as a list of regions, and the builders that lay
//! crafted tables out in one.
//!
//! [`Machine::byte`] panics on an address no `readable` call accepted. That is
//! deliberate and it is the instrument: the crate's contract is that it reads
//! nothing it has not first bounded, so an unbounded read here is a red rather
//! than a quietly wrong byte.

use toyos_acpi::Phys;

#[derive(Clone, Copy)]
pub struct Machine<'a> {
    pub regions: &'a [(u64, &'a [u8])],
}

impl<'a> Machine<'a> {
    fn at(self, phys: u64, len: usize) -> Option<&'a [u8]> {
        for (base, bytes) in self.regions {
            let end = base + bytes.len() as u64;
            if phys >= *base && phys.checked_add(len as u64).is_some_and(|e| e <= end) {
                return Some(&bytes[(phys - base) as usize..]);
            }
        }
        None
    }
}

impl Phys for Machine<'_> {
    fn readable(self, phys: u64, len: usize) -> bool {
        self.at(phys, len).is_some()
    }

    fn byte(self, phys: u64) -> u8 {
        match self.at(phys, 1) {
            Some(bytes) => bytes[0],
            None => panic!("the decoder read {phys:#x}, which no `readable` call accepted"),
        }
    }
}

/// Re-sum an SDT so its byte 9 makes the declared bytes add to zero.
pub fn reseal(table: &mut [u8]) {
    let len = u32::from_le_bytes([table[4], table[5], table[6], table[7]]) as usize;
    let len = len.min(table.len());
    table[9] = 0;
    let sum = table[..len].iter().fold(0u8, |a, b| a.wrapping_add(*b));
    table[9] = 0u8.wrapping_sub(sum);
}

/// Re-sum an RSDP on both of its checksums, for the length it declares.
pub fn reseal_rsdp(head: &mut [u8]) {
    head[8] = 0;
    let v1 = head[..20.min(head.len())].iter().fold(0u8, |a, b| a.wrapping_add(*b));
    head[8] = 0u8.wrapping_sub(v1);
    let len = u32::from_le_bytes([head[20], head[21], head[22], head[23]]) as usize;
    head[32] = 0;
    let whole = head[..len.min(head.len())].iter().fold(0u8, |a, b| a.wrapping_add(*b));
    head[32] = 0u8.wrapping_sub(whole);
}

/// A well-formed table: a 36-byte header over `body`, sealed.
pub fn sdt(signature: &[u8; 4], revision: u8, body: &[u8]) -> Vec<u8> {
    let mut t = vec![0u8; 36];
    t[0..4].copy_from_slice(signature);
    t[8] = revision;
    t.extend_from_slice(body);
    let len = t.len() as u32;
    t[4..8].copy_from_slice(&len.to_le_bytes());
    reseal(&mut t);
    t
}

/// Change what a table says its length is, keeping it sealed for that length.
pub fn declare_len(table: &mut [u8], len: u32) {
    table[4..8].copy_from_slice(&len.to_le_bytes());
    reseal(table);
}

/// An XSDT over `entries`.
pub fn xsdt(entries: &[u64]) -> Vec<u8> {
    let mut body = Vec::new();
    for e in entries {
        body.extend_from_slice(&e.to_le_bytes());
    }
    sdt(b"XSDT", 1, &body)
}

/// An RSDP naming `xsdt_addr`, sealed on both checksums for `len` bytes.
pub fn rsdp(xsdt_addr: u64, revision: u8, len: u32) -> Vec<u8> {
    let mut r = vec![0u8; 36];
    r[0..8].copy_from_slice(b"RSD PTR ");
    r[15] = revision;
    r[20..24].copy_from_slice(&len.to_le_bytes());
    r[24..32].copy_from_slice(&xsdt_addr.to_le_bytes());
    let v1 = r[..20].iter().fold(0u8, |a, b| a.wrapping_add(*b));
    r[8] = 0u8.wrapping_sub(v1);
    let whole = r[..(len as usize).min(36)].iter().fold(0u8, |a, b| a.wrapping_add(*b));
    r[32] = 0u8.wrapping_sub(whole);
    r
}

/// One MADT interrupt controller structure: type, length, then `body`.
pub fn entry(entry_type: u8, len: u8, body: &[u8]) -> Vec<u8> {
    let mut e = vec![entry_type, len];
    e.extend_from_slice(body);
    e
}

/// A MADT over `entries`, with the two header words ACPI 6.5 Table 5.19 puts
/// ahead of the structure list.
pub fn madt(entries: &[u8]) -> Vec<u8> {
    let mut body = vec![0u8; 8];
    body.extend_from_slice(entries);
    sdt(b"APIC", 5, &body)
}
