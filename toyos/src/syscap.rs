//! The capability whose whole authority is in the rights on the handle.
//!
//! Some things are reachable no other way — minting a device claim, entering
//! the real-time band, listing every process in the machine, reading what the
//! machine is made of, and taking its power away, off or back to firmware —
//! and each is one bit on a handle to this. The kernel makes exactly one at boot, for the supervisor, so the set of
//! processes that can ever do any of them is exactly what the supervisor endowed.

use toyos_abi::handle::Rights;
use toyos_abi::inventory::{RawRecord, Record, Undecodable};
use toyos_abi::syscall::{self, DeviceRequest, DeviceType, SyscallError};

use crate::endow::FromHandle;
use crate::{AsHandle, OwnedHandle, RawHandle};

pub struct SysCap(pub(crate) OwnedHandle);

impl SysCap {
    /// Mint the claim for a device class, as whichever typed wrapper the caller
    /// drives it through.
    ///
    /// `NotFound` is a machine with no such device, which is a fact the supervisor logs
    /// and endows nothing for — not a failure. `AlreadyExists` is another
    /// process holding the class, which is a different fact and stays loud.
    pub fn claim<T: FromHandle>(&self, class: DeviceType) -> Result<T, SyscallError> {
        self.mint(DeviceRequest::Class(class))
    }

    /// Mint the claim for one PCI function, named by what identifies the card.
    ///
    /// Apart from [`Self::claim`] rather than one call taking a
    /// [`DeviceRequest`], so there is no way to ask for
    /// [`DeviceType::PciFunction`] with no function named: a class is the whole
    /// of what the call above asks for, and this one cannot be asked without an
    /// id.
    pub fn claim_pci<T: FromHandle>(
        &self,
        id: toyos_abi::syscall::PciId,
    ) -> Result<T, SyscallError> {
        self.mint(DeviceRequest::Pci(id))
    }

    /// Mint the claim for one GPT partition, by its unique GUID. Apart from
    /// [`Self::claim`] for the reason [`Self::claim_pci`] is: this one cannot be
    /// asked without naming which.
    ///
    /// `PermissionDenied` is a partition the kernel has mounted, and
    /// `AlreadyExists` one another process holds: a partition has one holder.
    pub fn claim_partition<T: FromHandle>(
        &self,
        guid: toyos_abi::part::PartGuid,
    ) -> Result<T, SyscallError> {
        self.mint(DeviceRequest::Partition(guid))
    }

    /// Mint the claim for one legacy ISA function, by exactly its ports and
    /// lines. Apart from [`Self::claim`] for the reason [`Self::claim_pci`] is.
    ///
    /// `NotFound` is a set that is not one function this machine can hand out
    /// whole, and `PermissionDenied` one the kernel drives itself.
    pub fn claim_isa<T: FromHandle>(
        &self,
        set: toyos_abi::syscall::IsaId,
    ) -> Result<T, SyscallError> {
        self.mint(DeviceRequest::Isa(set))
    }

    fn mint<T: FromHandle>(&self, request: DeviceRequest) -> Result<T, SyscallError> {
        let raw = syscall::device_claim(self.0.raw(), request)?;
        // SAFETY: the kernel installed this handle in this process's table for
        // this call and no other, so nothing else answers for it.
        Ok(unsafe { T::from_handle(raw) })
    }

    /// A second handle to this capability, carrying the same rights.
    ///
    /// Only usable by a holder whose own cap carries [`Rights::DUP`], which in
    /// the whole tree is the test estate: its binaries mint their own claims,
    /// and one boot runs several that each need the keyboard.
    pub fn duplicate(&self) -> Result<Self, SyscallError> {
        syscall::dup(self.0.raw()).map(|h| Self(OwnedHandle(h)))
    }

    /// Enter the real-time band. A device claim was never enough to confer
    /// this; a right is.
    pub fn enter_rt(&self) -> Result<(), SyscallError> {
        syscall::rt_enter(self.0.raw())
    }

    /// Power the machine off.
    ///
    /// **Returns only when refused**, because a shutdown that happened has no
    /// caller left to answer. `PermissionDenied` is a capability that does not
    /// carry [`Rights::POWER`] — which is every capability in the machine but
    /// the ones a `system.toml` row named `power` in.
    pub fn shutdown(&self) -> SyscallError {
        syscall::shutdown(self.0.raw())
    }

    /// Back to firmware, on [`Self::shutdown`]'s right, refused the same way.
    pub fn reboot(&self) -> SyscallError {
        syscall::reboot(self.0.raw())
    }

    /// The machine's header, then one entry per live thread for as much of
    /// `buf` as is left. Answers the bytes written, and `0` if it was refused.
    ///
    /// Needs [`Rights::ROSTER`], which is what makes the entries — a pid, a
    /// size, a CPU time and a **name** for every process in the machine — the
    /// business of `ps` and not of every program that can make a syscall.
    /// [`crate::system::sysinfo`] is the header on its own and needs nothing.
    ///
    /// A `buf` too small for an entry asks for the header, which this
    /// capability is not needed for and is not consulted about.
    pub fn roster(&self, buf: &mut [u8]) -> usize {
        syscall::sysinfo(self.0.raw(), buf)
    }

    /// Every `toyos_abi::inventory` record the machine has, into `buf`;
    /// answers how many. An empty `buf` asks how many there are, and a `buf`
    /// too short for all of them is refused whole
    /// ([`SyscallError::ResourceExhausted`]) — see
    /// [`syscall::device_inventory`].
    ///
    /// Needs [`Rights::INVENTORY`].
    pub fn inventory(
        &self,
        buf: &mut [toyos_abi::inventory::RawRecord],
    ) -> Result<usize, SyscallError> {
        syscall::device_inventory(self.0.raw(), buf)
    }

    /// Every inventory record, read whole or refused whole, never a shorter
    /// list; `buffer(n)` is `n` records to read into.
    ///
    /// Needs [`Rights::INVENTORY`].
    pub fn records<B, C>(&self, buffer: impl FnMut(usize) -> B) -> Result<C, Unread>
    where
        B: AsMut<[RawRecord]>,
        C: FromIterator<Record>,
    {
        read_whole(|buf| self.inventory(buf), buffer)
    }

    /// A second handle to this capability carrying **less**.
    ///
    /// How the supervisor gives a program the RT band and nothing else: rights only
    /// shrink, so the dup can never mint a claim however the holder asks.
    pub fn narrowed(&self, rights: Rights) -> Result<Self, SyscallError> {
        syscall::dup_narrowed(self.0.raw(), rights).map(|h| Self(OwnedHandle(h)))
    }

    /// Give up ownership, for a handle about to be endowed.
    pub fn into_raw(self) -> RawHandle {
        self.0.into_raw()
    }
}

impl AsHandle for SysCap {
    fn as_handle(&self) -> RawHandle {
        self.0.raw()
    }
}

/// How many times [`SysCap::records`] asks again after the machine grew
/// between counting its inventory and reading it. Policy: a device arriving on
/// every round is a machine the reader names rather than chases.
const INVENTORY_ROUNDS: usize = 4;

/// Why [`SysCap::records`] did not read the inventory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unread {
    Count(SyscallError),
    Read(SyscallError),
    /// The machine grew between the count and the read on every round.
    Grew,
    Record { index: usize, why: Undecodable },
}

impl core::fmt::Display for Unread {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Count(SyscallError::PermissionDenied) | Self::Read(SyscallError::PermissionDenied) => {
                write!(f, "the kernel refused: this program's capability does not carry `inventory`")
            }
            Self::Count(e) => write!(f, "the inventory would not count: {e:?}"),
            Self::Read(e) => write!(f, "the inventory would not read: {e:?}"),
            Self::Grew => write!(f, "the machine changed on each of {INVENTORY_ROUNDS} reads of its inventory"),
            Self::Record { index, why } => write!(f, "the inventory's record {index} does not decode: {why}"),
        }
    }
}

/// Every record `ask` answers, where `ask` is the inventory call: an empty
/// buffer asks how many, and a buffer that long is filled or refused with
/// `ResourceExhausted` because the machine grew since.
fn read_whole<B, C>(
    mut ask: impl FnMut(&mut [RawRecord]) -> Result<usize, SyscallError>,
    mut buffer: impl FnMut(usize) -> B,
) -> Result<C, Unread>
where
    B: AsMut<[RawRecord]>,
    C: FromIterator<Record>,
{
    for _ in 0..INVENTORY_ROUNDS {
        let count = ask(&mut []).map_err(Unread::Count)?;
        // An empty buffer would ask the count again rather than read.
        if count == 0 {
            return Ok(core::iter::empty().collect());
        }
        let mut raw = buffer(count);
        let raw = raw.as_mut();
        assert_eq!(raw.len(), count, "`buffer({count})` answered {} records", raw.len());
        match ask(raw) {
            Ok(n) => {
                return raw[..n]
                    .iter()
                    .enumerate()
                    .map(|(index, r)| Record::decode(r).map_err(|why| Unread::Record { index, why }))
                    .collect();
            }
            Err(SyscallError::ResourceExhausted) => continue,
            Err(e) => return Err(Unread::Read(e)),
        }
    }
    Err(Unread::Grew)
}

#[cfg(test)]
mod tests {
    use super::*;
    use toyos_abi::inventory::{Loaded, Role};

    fn loaded(role: Role) -> Record {
        Record::Loaded(Loaded { role, unique_guid: [7; 16] })
    }

    fn read(ask: impl FnMut(&mut [RawRecord]) -> Result<usize, SyscallError>) -> Result<Vec<Record>, Unread> {
        read_whole(ask, |n| vec![RawRecord::EMPTY; n])
    }

    /// The kernel's answer to `buf` from a machine of `records`.
    fn answer(records: &[RawRecord], buf: &mut [RawRecord]) -> Result<usize, SyscallError> {
        match buf.len() {
            0 => Ok(records.len()),
            n if n < records.len() => Err(SyscallError::ResourceExhausted),
            _ => {
                buf[..records.len()].copy_from_slice(records);
                Ok(records.len())
            }
        }
    }

    #[test]
    fn every_record_is_read() {
        let records = [loaded(Role::Root), loaded(Role::Log)];
        let raw: Vec<RawRecord> = records.iter().map(Record::encode).collect();
        assert_eq!(read(|buf| answer(&raw, buf)), Ok(records.to_vec()));
    }

    #[test]
    fn a_machine_that_grew_once_is_read_grown() {
        let before = [loaded(Role::Root)];
        let after = [loaded(Role::Root), loaded(Role::Log)];
        let (before_raw, after_raw): (Vec<RawRecord>, Vec<RawRecord>) =
            (before.iter().map(Record::encode).collect(), after.iter().map(Record::encode).collect());
        let mut counted = false;
        let grows_after_the_first_count = |buf: &mut [RawRecord]| {
            let machine = if counted { &after_raw } else { &before_raw };
            counted |= buf.is_empty();
            answer(machine, buf)
        };
        assert_eq!(read(grows_after_the_first_count), Ok(after.to_vec()));
    }

    #[test]
    fn a_machine_that_grows_every_round_is_refused_by_name() {
        let mut machine = vec![loaded(Role::Root).encode()];
        let mut asks = 0;
        let grows_after_every_count = |buf: &mut [RawRecord]| {
            let answered = answer(&machine, buf);
            if buf.is_empty() {
                machine.push(loaded(Role::Boot).encode());
            }
            asks += 1;
            answered
        };
        assert_eq!(read(grows_after_every_count), Err(Unread::Grew));
        assert_eq!(asks, 2 * INVENTORY_ROUNDS, "every round counted and read once");
    }

    #[test]
    fn an_empty_inventory_is_its_count() {
        let mut asks = 0;
        let empty_then_grown = |buf: &mut [RawRecord]| {
            asks += 1;
            answer(&vec![loaded(Role::Root).encode(); asks - 1], buf)
        };
        assert_eq!(read(empty_then_grown), Ok(Vec::new()));
        assert_eq!(asks, 1, "the count was the whole answer");
    }

    #[test]
    fn a_refused_count_is_refused_and_not_an_empty_inventory() {
        assert_eq!(read(|_| Err(SyscallError::PermissionDenied)), Err(Unread::Count(SyscallError::PermissionDenied)));
    }

    #[test]
    fn a_refused_read_is_refused_and_not_an_empty_inventory() {
        let refused = |buf: &mut [RawRecord]| match buf.len() {
            0 => Ok(1),
            _ => Err(SyscallError::PermissionDenied),
        };
        assert_eq!(read(refused), Err(Unread::Read(SyscallError::PermissionDenied)));
    }

    #[test]
    fn a_record_that_does_not_decode_is_refused_and_not_dropped() {
        let raw = [loaded(Role::Root).encode(), RawRecord::EMPTY, loaded(Role::Boot).encode()];
        assert_eq!(read(|buf| answer(&raw, buf)), Err(Unread::Record { index: 1, why: Undecodable::Kind(0) }));
    }
}
