//! One process's handle table.
//!
//! Lives behind `ProcessData`'s own lock; every accessor returns an owned
//! value, so no borrow into the table outlives the guard.
//!
//! Not a file-descriptor table — `fd` names only `userland/libc`'s POSIX
//! surface (owner ruling) — and a slot at its last generation retires
//! rather than wrapping, so a stale handle can never resolve again.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::SyscallError;

use super::{KObjectRef, KObjectVariant};

/// The table has no slot left — its own type since `install` runs under the process's own lock, where [`HandleError::refuse`] must not be called.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TableFull;

/// Why a handle did not resolve.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HandleError {
    /// Out of range, or an empty slot.
    BadHandle,
    /// The slot has moved past this handle: closed, or its generations ran out.
    Stale,
    /// Not the caller's bug at `SYS_NAMESPACE_BUILD`'s connector argument — the sole named exception.
    WrongType { held: &'static str, wanted: &'static str },
    /// The handle is fine and does not carry what the call needs.
    Rights { held: Rights, needed: Rights },
    TableFull,
}

impl HandleError {
    /// Answer this failure at the syscall boundary; call with nothing held.
    pub fn refuse(self) -> u64 {
        self.refuse_as_error().to_u64()
    }

    /// [`refuse`](Self::refuse) for a `Result`-returning call site: same rule.
    pub fn refuse_as_error(self) -> SyscallError {
        match self {
            Self::Rights { .. } => SyscallError::PermissionDenied,
            Self::TableFull => SyscallError::ResourceExhausted,
            fault => crate::process::handle_fault(fault),
        }
    }
}

/// A refusal carried out of a lock guard, to be resolved with nothing held.
pub enum Refusal {
    Handle(HandleError),
    Error(SyscallError),
}

impl Refusal {
    /// See [`HandleError::refuse`]: nothing held.
    pub fn refuse(self) -> u64 {
        match self {
            Self::Handle(e) => e.refuse(),
            Self::Error(e) => e.to_u64(),
        }
    }
}

impl From<HandleError> for Refusal {
    fn from(e: HandleError) -> Self {
        Self::Handle(e)
    }
}

impl From<SyscallError> for Refusal {
    fn from(e: SyscallError) -> Self {
        Self::Error(e)
    }
}

impl core::fmt::Display for HandleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadHandle => write!(f, "no such handle"),
            Self::Stale => write!(f, "a handle closed at an earlier generation"),
            Self::WrongType { held, wanted } => {
                write!(f, "a {held} where the call takes a {wanted}")
            }
            Self::Rights { held, needed } => {
                write!(f, "rights {:#x} where the call needs {:#x}", held.bits(), needed.bits())
            }
            Self::TableFull => write!(f, "no free handle slot"),
        }
    }
}

/// One handle: what it names and what it may do to it.
// Not `Clone`: a second entry for one slot would double the `handle_count`.
pub struct HandleEntry {
    object: KObjectRef,
    rights: Rights,
}

impl HandleEntry {
    /// The only constructor; resurrecting an already-retired object is a kernel bug.
    pub fn new(object: KObjectRef, rights: Rights) -> Self {
        let core = object.core();
        assert!(
            !core.retired(),
            "a handle to a retired {} (koid {})",
            object.kind(),
            core.koid().raw(),
        );
        core.handle_count.fetch_add(1, Ordering::AcqRel);
        Self { object, rights }
    }

    pub fn object(&self) -> &KObjectRef {
        &self.object
    }

    /// A second handle to the same object, carrying no more than this one.
    pub fn duplicate(&self, rights: Rights) -> Result<Self, HandleError> {
        if !self.rights.contains(Rights::DUP) {
            return Err(HandleError::Rights { held: self.rights, needed: Rights::DUP });
        }
        if !rights.subset_of(self.rights) {
            return Err(HandleError::Rights { held: self.rights, needed: rights });
        }
        Ok(Self::new(self.object.clone(), rights))
    }
}

impl Drop for HandleEntry {
    fn drop(&mut self) {
        let core = self.object.core();
        if core.handle_count.fetch_sub(1, Ordering::AcqRel) == 1 {
            let first = !core.retired.swap(true, Ordering::AcqRel);
            assert!(
                first,
                "handle_count resurrected after zero on {} (koid {})",
                self.object.kind(),
                core.koid().raw(),
            );
            // Deferred, not run inline: a release hook must never run under this lock.
            if self.object.defers_release() {
                super::enqueue_zero_handles(self.object.clone());
            }
        }
    }
}

/// One slot of a process's table: allocatable at a generation, or retired for good.
enum Slot {
    /// Issuable at this generation; `entry` says whether anything is in it.
    Serving { generation: u32, entry: Option<HandleEntry> },
    /// Spent: never issued or written again — the table is permanently one slot smaller.
    Retired,
}

/// Handles one process may hold.
pub const MAX_HANDLES: usize = RawHandle::MAX_SLOTS;

pub struct HandleTable {
    slots: Vec<Slot>,
    // A retired slot is never on this list — it is simply never offered again.
    free: Vec<u16>,
}

impl HandleTable {
    pub const fn new() -> Self {
        Self { slots: Vec::new(), free: Vec::new() }
    }

    /// Whether `n` more `install`s can all succeed.
    // A retired slot counts in neither term, so it permanently costs one slot of room.
    pub fn has_room(&self, n: usize) -> bool {
        self.free.len() + (MAX_HANDLES - self.slots.len()) >= n
    }

    pub fn install(&mut self, entry: HandleEntry) -> Result<RawHandle, TableFull> {
        if let Some(slot) = self.free.pop() {
            // The free list never holds a retired or occupied slot, so this is always a vacancy.
            let Slot::Serving { generation, entry: vacancy } = &mut self.slots[slot as usize]
            else {
                unreachable!("the free list offered a retired slot");
            };
            debug_assert!(vacancy.is_none(), "a live slot was on the free list");
            *vacancy = Some(entry);
            return Ok(RawHandle::new(slot, *generation));
        }
        if self.slots.len() >= MAX_HANDLES {
            return Err(TableFull);
        }
        let slot = self.slots.len() as u16;
        self.slots.push(Slot::Serving { generation: 0, entry: Some(entry) });
        Ok(RawHandle::new(slot, 0))
    }

    /// Install every object all-or-nothing: a batch that would overflow the
    /// table is refused whole, leaving the table exactly as it was found.
    // `has_room` reserves the slots before any `HandleEntry::new`, so a refusal
    // builds and installs nothing — undoing a partial install would retire the objects.
    pub fn install_all(
        &mut self,
        objects: Vec<(KObjectRef, Rights)>,
    ) -> Result<Vec<RawHandle>, TableFull> {
        if !self.has_room(objects.len()) {
            return Err(TableFull);
        }
        Ok(objects
            .into_iter()
            .map(|(object, rights)| {
                self.install(HandleEntry::new(object, rights))
                    .expect("has_room reserved every slot")
            })
            .collect())
    }

    /// Install at a caller-chosen slot, replacing whatever is there.
    #[must_use = "the displaced entry must be dropped by the caller"]
    pub fn install_at(
        &mut self,
        slot: u16,
        entry: HandleEntry,
    ) -> Result<(RawHandle, Option<HandleEntry>), TableFull> {
        let slot_index = slot as usize;
        // A slot past MAX_HANDLES is the table's cap, not a malformed argument.
        if slot_index >= MAX_HANDLES {
            return Err(TableFull);
        }
        while self.slots.len() <= slot_index {
            self.free.push(self.slots.len() as u16);
            self.slots.push(Slot::Serving { generation: 0, entry: None });
        }
        self.free.retain(|&s| s != slot);
        // A retired slot answers `TableFull` too: the table no longer has that slot at all.
        let Slot::Serving { generation, entry: at } = &mut self.slots[slot_index] else {
            return Err(TableFull);
        };
        // Generation stays put: dup2 keeps the number the caller already holds valid.
        let displaced = at.replace(entry);
        Ok((RawHandle::new(slot, *generation), displaced))
    }

    /// The entry a handle names, or why it names none; a retired slot is `Stale`, not `BadHandle`, since the slot is in range.
    fn entry_of(&self, h: RawHandle) -> Result<&HandleEntry, HandleError> {
        match self.slots.get(h.slot() as usize).ok_or(HandleError::BadHandle)? {
            Slot::Retired => Err(HandleError::Stale),
            Slot::Serving { generation, .. } if *generation != h.generation() => {
                Err(HandleError::Stale)
            }
            Slot::Serving { entry, .. } => entry.as_ref().ok_or(HandleError::BadHandle),
        }
    }

    /// The typed accessor: an owned `Arc` that outlives the guard.
    pub fn get<T: KObjectVariant>(
        &self,
        h: RawHandle,
        need: Rights,
    ) -> Result<Arc<T>, HandleError> {
        let entry = self.entry_of(h)?;
        if !entry.rights.contains(need) {
            return Err(HandleError::Rights { held: entry.rights, needed: need });
        }
        T::from_ref(&entry.object)
            .cloned()
            .ok_or(HandleError::WrongType { held: entry.object.kind(), wanted: T::NAME })
    }

    /// The borrowing accessor, for a call that completes while still holding the guard.
    pub fn get_ref(&self, h: RawHandle, need: Rights) -> Result<&KObjectRef, HandleError> {
        let entry = self.entry_of(h)?;
        if !entry.rights.contains(need) {
            return Err(HandleError::Rights { held: entry.rights, needed: need });
        }
        Ok(&entry.object)
    }

    pub fn duplicate(
        &mut self,
        h: RawHandle,
        rights: Rights,
    ) -> Result<RawHandle, HandleError> {
        let entry = self.entry_of(h)?.duplicate(rights)?;
        self.install(entry).map_err(|TableFull| HandleError::TableFull)
    }

    /// What a handle carries, for a caller about to duplicate it unchanged.
    pub fn rights_of(&self, h: RawHandle) -> Result<Rights, HandleError> {
        Ok(self.entry_of(h)?.rights)
    }

    /// A duplicate for *another* table — a child's, built at spawn.
    pub fn duplicate_entry(
        &self,
        h: RawHandle,
        rights: Rights,
    ) -> Result<HandleEntry, HandleError> {
        self.entry_of(h)?.duplicate(rights)
    }

    /// Take a handle out of the table and retire its slot.
    #[must_use = "the removed entry must be dropped by the caller"]
    pub fn remove(&mut self, h: RawHandle) -> Result<HandleEntry, HandleError> {
        let entry = self.take_for_transfer(h)?;
        self.retire(h);
        Ok(entry)
    }

    /// Take an entry out, leaving its slot claimed at its own generation.
    #[must_use = "the entry must be given back or its slot retired"]
    fn take_for_transfer(&mut self, h: RawHandle) -> Result<HandleEntry, HandleError> {
        match self.slots.get_mut(h.slot() as usize).ok_or(HandleError::BadHandle)? {
            Slot::Retired => Err(HandleError::Stale),
            Slot::Serving { generation, .. } if *generation != h.generation() => {
                Err(HandleError::Stale)
            }
            Slot::Serving { entry, .. } => entry.take().ok_or(HandleError::BadHandle),
        }
    }

    /// A slot retires at its last generation instead of advancing, keeping `HANDLE_INVALID` (slot 4095 at `MAX_GENERATION`) unissuable.
    fn retire(&mut self, h: RawHandle) {
        let index = h.slot() as usize;
        let spent = match &mut self.slots[index] {
            Slot::Serving { generation, entry } => {
                debug_assert!(entry.is_none(), "a slot still holding an entry was retired");
                if *generation == RawHandle::MAX_GENERATION - 1 {
                    true
                } else {
                    *generation += 1;
                    false
                }
            }
            Slot::Retired => unreachable!("a retired slot answered a handle"),
        };
        if spent {
            self.slots[index] = Slot::Retired;
        } else {
            self.free.push(h.slot());
        }
    }

    /// Put an entry back at the number it was taken from.
    fn give_back(&mut self, h: RawHandle, entry: HandleEntry) {
        let Slot::Serving { generation, entry: vacancy } = &mut self.slots[h.slot() as usize]
        else {
            unreachable!("a slot taken for transfer was retired under the same lock");
        };
        debug_assert!(
            vacancy.is_none() && *generation == h.generation(),
            "a slot taken for transfer was written under the same lock",
        );
        *vacancy = Some(entry);
    }

    /// Move `handles` into `sink`; if `sink` refuses, every handle is restored to its own slot.
    // A refused transfer must not drop the entries: the caller was told nothing happened.
    pub fn transfer<E>(
        &mut self,
        handles: &[RawHandle],
        sink: impl FnOnce(Vec<HandleEntry>) -> Result<(), (Vec<HandleEntry>, E)>,
    ) -> Result<(), E> {
        let mut batch = Vec::with_capacity(handles.len());
        for h in handles {
            batch.push(
                self.take_for_transfer(*h).expect("a handle verified under this same hold"),
            );
        }
        match sink(batch) {
            Ok(()) => {
                for h in handles {
                    self.retire(*h);
                }
                Ok(())
            }
            Err((batch, e)) => {
                for (h, entry) in handles.iter().zip(batch) {
                    self.give_back(*h, entry);
                }
                Err(e)
            }
        }
    }

    /// Empty the table; the caller drops what it gets with nothing held.
    #[must_use = "the drained entries must be dropped by the caller"]
    pub fn drain(&mut self) -> Vec<HandleEntry> {
        let mut out = Vec::new();
        for slot in &mut self.slots {
            match slot {
                Slot::Serving { entry, .. } => out.extend(entry.take()),
                // A retired slot's entry was already taken when it retired.
                Slot::Retired => {}
            }
        }
        self.free.clear();
        out
    }

    pub fn iter(&self) -> impl Iterator<Item = (RawHandle, &HandleEntry)> {
        self.slots.iter().enumerate().filter_map(|(i, slot)| match slot {
            Slot::Serving { generation, entry } => {
                entry.as_ref().map(|e| (RawHandle::new(i as u16, *generation), e))
            }
            Slot::Retired => None,
        })
    }

    /// Self-test: fill the table to `room` slots of capacity with empty,
    // non-free slots `install` can neither reuse nor grow past.
    #[cfg(feature = "boot-actuators")]
    pub fn stage_room(&mut self, room: usize) {
        while self.slots.len() < MAX_HANDLES - room {
            self.slots.push(Slot::Serving { generation: 0, entry: None });
        }
    }

    /// Test actuator: park a free slot at its last generation and answer the handle its next install will carry.
    // Real exhaustion needs a generation's worth of close/install cycles; this stages the field directly.
    #[cfg(feature = "test-actuators")]
    pub fn stage_last_generation(&mut self, slot: u16) -> Option<RawHandle> {
        match self.slots.get_mut(slot as usize)? {
            Slot::Serving { generation, entry: None } => {
                *generation = RawHandle::MAX_GENERATION - 1;
                Some(RawHandle::new(slot, *generation))
            }
            Slot::Serving { entry: Some(_), .. } | Slot::Retired => None,
        }
    }
}
