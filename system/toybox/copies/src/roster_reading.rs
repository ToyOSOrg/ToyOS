//! The roster `ps` reads: a refused one is no row, a granted one its
//! header and each whole entry.

use toyos_abi::syscall::{SYSINFO_ENTRY_SIZE as ENTRY, SYSINFO_HEADER_SIZE as HEADER};

use crate::roster::*;

/// A refused roster is nothing at all: no header and no row.
#[test]
fn a_refused_roster_is_no_row() {
    assert_eq!(rows(&[]), None);
    assert_eq!(rows(&[0; HEADER - 1]), None, "short of a header is refused as well");
}

/// A roster the kernel granted: the header's two clocks, then each whole
/// entry; a trailing part of one is no row.
#[test]
fn a_granted_roster_is_its_header_and_each_whole_entry() {
    let mut answer = vec![0u8; HEADER + ENTRY + ENTRY / 2];
    let header = toyos_abi::syscall::SysinfoHeader::decode(answer[..HEADER].try_into().unwrap());
    let entry = &mut answer[HEADER..HEADER + ENTRY];
    entry[..4].copy_from_slice(&7u32.to_le_bytes());
    entry[4..8].copy_from_slice(&1u32.to_le_bytes());
    entry[32..37].copy_from_slice(b"shell");
    let (uptime, cpu, rows) = rows(&answer).expect("a granted roster");
    assert_eq!((uptime, cpu), (header.uptime_ns, header.total_cpu_ns));
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].pid, rows[0].tid, rows[0].name.as_str()), (7, 1, "shell"));
}
