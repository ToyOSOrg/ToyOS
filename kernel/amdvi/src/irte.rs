//! A requester's interrupt remapping table entry in its 128-bit format
//! (§2.2.5.2, Figure 17 and Table 22, `GuestMode` 0; with `XTEn`, Figure 19):
//! selected by `CONTROL.GAEn` with `GAMEn` 000b (Table 21). A fixed message's
//! data bits 10:0 index the table (Figure 14), so with delivery mode 000b the
//! entry is the message's own vector.

const REMAP_EN: u64 = 1 << 0;
const DESTINATION_SHIFT: u32 = 8;
/// `Vector`, bits 71:64: bits 7:0 of the second doubleword.
const VECTOR_SHIFT: u32 = 0;

/// Where an entry delivers: the whole x2APIC id under `XTEn`, and without it
/// an 8-bit APIC id, bits 31:16 then reserved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Destination {
    Extended(u32),
    Narrow(u8),
}

/// An entry delivering `vector` to `destination`, fixed (`IntType` 000b),
/// physical (`DM` 0), no EOI requested; or [`ABSENT`], whose message is
/// target-aborted and recorded (`RemapEn` 0).
pub const fn entry(vector: u8, destination: Destination) -> [u64; 2] {
    let (low, high) = match destination {
        Destination::Extended(id) => ((id as u64 & 0xFF_FFFF) << DESTINATION_SHIFT, (id as u64 >> 24) << 56),
        Destination::Narrow(id) => ((id as u64) << DESTINATION_SHIFT, 0),
    };
    [REMAP_EN | low, (vector as u64) << VECTOR_SHIFT | high]
}

pub const ABSENT: [u64; 2] = [0, 0];

/// Whether an entry read back delivers anything.
pub const fn present(words: [u64; 2]) -> bool {
    words[0] & REMAP_EN != 0
}

/// The vector an entry read back delivers.
pub const fn vector(words: [u64; 2]) -> u8 {
    (words[1] >> VECTOR_SHIFT) as u8
}

/// The destination an entry read back delivers to, all 32 bits.
pub const fn destination(words: [u64; 2]) -> u32 {
    ((words[0] >> DESTINATION_SHIFT) & 0xFF_FFFF) as u32 | ((words[1] >> 56) as u32) << 24
}
