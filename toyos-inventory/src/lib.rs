//! The kernel's device inventory, read whole: a count, then that many records,
//! each of which decodes. A refused count, a refused read and a record that
//! does not decode are each the answer, never a shorter list.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use toyos_abi::inventory::{RawRecord, Record, Undecodable};
use toyos_abi::syscall::SyscallError;

/// Why the inventory was not read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unread {
    Count(SyscallError),
    Read(SyscallError),
    Record { index: usize, why: Undecodable },
}

impl core::fmt::Display for Unread {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Count(e) => write!(f, "the inventory would not count: {e:?}"),
            Self::Read(e) => write!(f, "the inventory would not read: {e:?}"),
            Self::Record { index, why } => write!(f, "the inventory's record {index} does not decode: {why}"),
        }
    }
}

/// Every record `ask` answers, where `ask` is the inventory call: an empty
/// buffer asks how many, and a buffer that long is filled.
pub fn read(mut ask: impl FnMut(&mut [RawRecord]) -> Result<usize, SyscallError>) -> Result<Vec<Record>, Unread> {
    let asked = ask(&mut []).map_err(Unread::Count)?;
    let mut raw = vec![RawRecord::EMPTY; asked];
    let n = ask(&mut raw).map_err(Unread::Read)?;
    raw[..n]
        .iter()
        .enumerate()
        .map(|(index, r)| Record::decode(r).map_err(|why| Unread::Record { index, why }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use toyos_abi::inventory::{Loaded, Role};

    fn loaded(role: Role) -> Record {
        Record::Loaded(Loaded { role, unique_guid: [7; 16] })
    }

    /// An inventory of `records`, as the kernel answers one.
    fn kernel(records: Vec<RawRecord>) -> impl FnMut(&mut [RawRecord]) -> Result<usize, SyscallError> {
        move |buf| match buf.len() {
            0 => Ok(records.len()),
            n if n < records.len() => Err(SyscallError::ResourceExhausted),
            _ => {
                buf[..records.len()].copy_from_slice(&records);
                Ok(records.len())
            }
        }
    }

    #[test]
    fn every_record_is_read() {
        let records = [loaded(Role::Root), loaded(Role::Log)];
        assert_eq!(read(kernel(records.iter().map(Record::encode).collect())), Ok(records.to_vec()));
    }

    #[test]
    fn a_refused_count_is_refused_and_not_an_empty_inventory() {
        assert_eq!(read(|_| Err(SyscallError::PermissionDenied)), Err(Unread::Count(SyscallError::PermissionDenied)));
    }

    #[test]
    fn a_refused_read_is_refused_and_not_an_empty_inventory() {
        let refused = |buf: &mut [RawRecord]| match buf.len() {
            0 => Ok(1),
            _ => Err(SyscallError::ResourceExhausted),
        };
        assert_eq!(read(refused), Err(Unread::Read(SyscallError::ResourceExhausted)));
    }

    #[test]
    fn a_record_that_does_not_decode_is_refused_and_not_dropped() {
        let records = vec![loaded(Role::Root).encode(), RawRecord::EMPTY, loaded(Role::Boot).encode()];
        assert_eq!(read(kernel(records)), Err(Unread::Record { index: 1, why: Undecodable::Kind(0) }));
    }
}
