//! AArch64's encoding of a [`Plan`](crate::Plan): the VMSAv8-64 stage 1
//! descriptors of a 4 KiB granule (Arm ARM K.a, D8.3), and the one `MAIR_EL1`
//! whose indices they name — the loader writes the one, the kernel's entry
//! loads the other, and both read them here.

use crate::Cache;

/// `MAIR_EL1` index 0: Device-nGnRE, registers.
pub const ATTR_DEVICE: u64 = 0;
/// `MAIR_EL1` index 1: Normal, inner and outer write-back, read- and
/// write-allocate — RAM.
pub const ATTR_NORMAL: u64 = 1;
/// `MAIR_EL1` index 2: Normal, inner and outer non-cacheable — the scanout,
/// whose stores gather as write-combining ones do.
pub const ATTR_NORMAL_NC: u64 = 2;
/// Each index's attribute byte (D24.2.110), in index order: the value
/// `PAR_EL1.ATTR` also reports a translation's type in.
pub const ATTRS: [u8; 3] = [0x04, 0xFF, 0x44];
/// `MAIR_EL1` whole.
pub const MAIR: u64 = ATTRS[0] as u64 | (ATTRS[1] as u64) << 8 | (ATTRS[2] as u64) << 16;

/// A table descriptor (bits 1:0 = 0b11) naming the next table down.
const TABLE: u64 = 0b11;
/// A block descriptor (bits 1:0 = 0b01) at level 1 or 2.
const BLOCK: u64 = 0b01;
/// `SH` = inner shareable, bits 9:8.
const INNER_SHAREABLE: u64 = 0b11 << 8;
/// `SH` = outer shareable.
const OUTER_SHAREABLE: u64 = 0b10 << 8;
/// The access flag, bit 10: set, so the first access does not fault.
const AF: u64 = 1 << 10;
/// Privileged execute-never, bit 53.
const PXN: u64 = 1 << 53;
/// Unprivileged execute-never, bit 54.
const UXN: u64 = 1 << 54;

/// A page descriptor (bits 1:0 = 0b11) at level 3.
const PAGE: u64 = 0b11;

/// A descriptor naming the next table down, at `phys`.
pub const fn table(phys: u64) -> u64 {
    phys | TABLE
}

/// A 2 MiB block at `phys`: EL1 read-write and EL0 nothing (`AP` = 0b00), and
/// executable at EL1 only where it is memory — the kernel's image is. A
/// device is never executable, since a speculative fetch from one is a read
/// of its registers.
pub const fn block(phys: u64, cache: Cache) -> u64 {
    phys | BLOCK | AF | attributes(cache)
}

const fn attributes(cache: Cache) -> u64 {
    match cache {
        Cache::Memory => ATTR_NORMAL << 2 | INNER_SHAREABLE | UXN,
        Cache::Device => ATTR_DEVICE << 2 | PXN | UXN,
        Cache::Scanout => ATTR_NORMAL_NC << 2 | OUTER_SHAREABLE | PXN | UXN,
        Cache::Firmware => panic!("AArch64 has no range registers to type memory: its plan types by the map"),
    }
}

/// A 4 KiB page at `phys`, with [`block`]'s attributes.
pub const fn page(phys: u64, cache: Cache) -> u64 {
    phys | PAGE | AF | attributes(cache)
}
