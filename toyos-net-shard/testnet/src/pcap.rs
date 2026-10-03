//! The classic capture file format every packet reader opens: a 24-byte file header naming
//! Ethernet, then each frame behind a 16-byte record header, all little-endian, stamped in
//! microseconds.

use toyos_net_wire::Instant;

const MAGIC_MICROSECONDS: u32 = 0xa1b2_c3d4;
const VERSION: (u16, u16) = (2, 4);
const SNAPLEN: u32 = 65_535;
const LINKTYPE_ETHERNET: u32 = 1;

/// A capture file of `frames`, each whole as it was on the wire at its instant.
pub fn pcap<'a>(frames: impl IntoIterator<Item = (Instant, &'a [u8])>) -> Vec<u8> {
    let mut file = Vec::new();
    file.extend_from_slice(&MAGIC_MICROSECONDS.to_le_bytes());
    file.extend_from_slice(&VERSION.0.to_le_bytes());
    file.extend_from_slice(&VERSION.1.to_le_bytes());
    file.extend_from_slice(&0i32.to_le_bytes());
    file.extend_from_slice(&0u32.to_le_bytes());
    file.extend_from_slice(&SNAPLEN.to_le_bytes());
    file.extend_from_slice(&LINKTYPE_ETHERNET.to_le_bytes());
    for (at, frame) in frames {
        let nanos = at.nanos();
        let seconds = u32::try_from(nanos / 1_000_000_000).expect("a capture ends before 2106");
        let micros = u32::try_from(nanos % 1_000_000_000 / 1_000).expect("below a million");
        let len = u32::try_from(frame.len()).ok().filter(|&len| len <= SNAPLEN).expect("a frame within the snap length");
        for field in [seconds, micros, len, len] {
            file.extend_from_slice(&field.to_le_bytes());
        }
        file.extend_from_slice(frame);
    }
    file
}
