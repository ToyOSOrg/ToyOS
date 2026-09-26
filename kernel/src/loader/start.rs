//! Loads a built process onto a CPU and builds the handle table it starts
//! with. The frame a new stack starts from and the trampolines it returns into
//! are the architecture's (`arch::entry`).

use alloc::vec::Vec;

use crate::object::{HandleTable, Refusal};
use crate::process::{
    process_data, Endowments, OwnedAlloc, ENDOW_ENTRY_LEN, KERNEL_STACK_SIZE,
};
use crate::scheduler;
use crate::user_ptr::UserBytes;
use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::{EndowEntry, SyscallError, MAX_ENDOWMENTS, MAX_LABELS_LEN, MAX_SLOT_MAP};

/// One `[child_slot, parent_handle]` pair of `SpawnArgs::slot_map_ptr`, in bytes.
pub const SLOT_PAIR_LEN: usize = 8;

/// Allocate a kernel stack and lay out the frame `context_switch` will restore.
pub(crate) fn alloc_kernel_stack(
    trampoline: unsafe extern "C" fn(),
    user_entry: u64,
    user_sp: u64,
    arg: u64,
) -> Option<(OwnedAlloc, u64)> {
    let alloc = OwnedAlloc::new(KERNEL_STACK_SIZE, 4096)?;
    scheduler::write_stack_canary(&alloc);
    let top = alloc.ptr() as u64 + KERNEL_STACK_SIZE as u64;
    // SAFETY: `alloc` is fresh and exclusively owned, and `top` is its end.
    let frame = unsafe { crate::arch::entry::initial_frame(top, trampoline, user_entry, user_sp, arg) };
    Some((alloc, frame))
}

/// The last path component, truncated to what a process entry can hold.
pub(crate) fn make_name(path: &str) -> [u8; crate::process::THREAD_NAME_LEN] {
    let filename = path.rsplit('/').next().unwrap_or(path);
    let mut name = [0u8; crate::process::THREAD_NAME_LEN];
    let len = filename.len().min(crate::process::THREAD_NAME_LEN - 1);
    name[..len].copy_from_slice(&filename.as_bytes()[..len]);
    name
}

/// A child's table, and the endowments that have not left the parent yet.
// The move must be last: an earlier move leaves a failed spawn's parent holding handles that name nothing.
pub enum PendingHandles {
    /// Built by the kernel and owing nobody anything — the boot's `/system/bin/init`.
    Ready(HandleTable, Endowments),
    /// A caller's request: `endow` has not left the caller's table yet.
    Moving { table: HandleTable, endow: Vec<u8>, labels: Vec<u8> },
}

impl PendingHandles {
    /// Take the endowed handles out of the parent's table, all under one lock hold: a refusal leaves it unchanged.
    pub fn commit(self) -> Result<(HandleTable, Endowments), Refusal> {
        let (mut table, endow, labels) = match self {
            Self::Ready(table, endowments) => return Ok((table, endowments)),
            Self::Moving { table, endow, labels } => (table, endow, labels),
        };
        let data_arc = process_data();
        let mut data = data_arc.lock();

        let count = endow.len() / ENDOW_ENTRY_LEN;
        let mut moving: Vec<(EndowEntry, RawHandle)> = Vec::with_capacity(count);
        for raw in endow.as_chunks::<ENDOW_ENTRY_LEN>().0 {
            let label_off = u32::from_ne_bytes([raw[0], raw[1], raw[2], raw[3]]);
            let label_len = u32::from_ne_bytes([raw[4], raw[5], raw[6], raw[7]]);
            let handle = RawHandle(u32::from_ne_bytes([raw[8], raw[9], raw[10], raw[11]]));
            let end = (label_off as usize)
                .checked_add(label_len as usize)
                .ok_or(SyscallError::InvalidArgument)?;
            if end > labels.len() {
                return Err(SyscallError::InvalidArgument.into());
            }
            // Checked before any removal, so a missing `TRANSFER` refuses the spawn instead of leaving a hole.
            let rights = data.handles.rights_of(handle)?;
            if !rights.contains(Rights::TRANSFER) {
                return Err(SyscallError::PermissionDenied.into());
            }
            // A duplicate is refused here: unremoved handles let it pass twice, then the second removal's
            // `expect` below panics.
            if moving.iter().any(|(_, seen)| *seen == handle) {
                return Err(SyscallError::InvalidArgument.into());
            }
            moving.push((EndowEntry { label_off, label_len, handle, _pad: 0 }, handle));
        }
        // Checked before any removal, so a failed install can't strand a handle out of a table that never spawned.
        if !table.has_room(moving.len()) {
            return Err(SyscallError::ResourceExhausted.into());
        }

        let mut entries = Vec::with_capacity(moving.len());
        for (mut entry, parent_handle) in moving {
            let moved = data
                .handles
                .remove(parent_handle)
                .expect("an endowed handle verified under this lock stopped resolving");
            entry.handle = table
                .install(moved)
                .expect("a child table with verified room refused an endowment");
            entries.push(entry);
        }
        Ok((table, Endowments::new(entries, labels)))
    }
}

/// Reads `SpawnArgs`'s two handle vectors into the child's pending handle state.
// Every refusal here precedes any child state, so nothing is enqueued or orphaned.
pub fn build_child_handles(
    slot_map: &UserBytes,
    endow: &UserBytes,
    labels: &[u8],
) -> Result<PendingHandles, Refusal> {
    if endow.len() / ENDOW_ENTRY_LEN > MAX_ENDOWMENTS {
        return Err(SyscallError::InvalidArgument.into());
    }
    // Checked before the loop: `install_at`'s cap misses a repeated slot, which would duplicate
    // handles under the parent's lock without bound.
    if slot_map.len() / SLOT_PAIR_LEN > MAX_SLOT_MAP {
        return Err(SyscallError::InvalidArgument.into());
    }
    if labels.len() > MAX_LABELS_LEN {
        return Err(SyscallError::InvalidArgument.into());
    }
    let data_arc = process_data();
    let data = data_arc.lock();
    let mut handles = HandleTable::new();
    // Dropped after the lock releases, not inside it: `install_at` leaves *where* to the caller.
    let mut displaced = Vec::new();
    // Parent console objects already minted a replacement for, keyed by address.
    let mut minted: Vec<(usize, crate::object::KObjectRef)> = Vec::new();
    for i in 0..slot_map.len() / SLOT_PAIR_LEN {
        let mut pair = [0u8; SLOT_PAIR_LEN];
        slot_map.read_at(i * SLOT_PAIR_LEN, &mut pair);
        let child_slot = u32::from_ne_bytes([pair[0], pair[1], pair[2], pair[3]]);
        let parent = RawHandle(u32::from_ne_bytes([pair[4], pair[5], pair[6], pair[7]]));
        // An unheld handle ends the parent (`object::HandleError`'s rule) rather than silently skipping the slot.
        let rights = data.handles.rights_of(parent)?;
        // A device claim carries no `DUP` and is refused by name.
        let entry = data.handles.duplicate_entry(parent, rights)?;
        // A console is minted, not duplicated: it is its own line buffer, and sharing one would splice
        // two half-lines together.
        let entry = match entry.object() {
            crate::object::KObjectRef::Console(parent_console) => {
                // Two slots naming the same parent console land on one child console — what this
                // address-keyed lookup preserves — or aliased stdout/stderr would split into two buffers.
                let key = alloc::sync::Arc::as_ptr(parent_console) as usize;
                let object = match minted.iter().find(|(seen, _)| *seen == key) {
                    Some((_, object)) => object.clone(),
                    None => {
                        let object = crate::object::KObjectRef::Console(
                            crate::object::device::ConsoleObject::new(),
                        );
                        minted.push((key, object.clone()));
                        object
                    }
                };
                // The duplicate is displaced too, dropped outside the parent's lock.
                displaced.push(Some(entry));
                crate::object::HandleEntry::new(object, rights)
            }
            _ => entry,
        };
        let slot = u16::try_from(child_slot)
            .map_err(|_| SyscallError::ResourceExhausted)?;
        let (_, replaced) = handles
            .install_at(slot, entry)
            .map_err(|_| SyscallError::ResourceExhausted)?;
        displaced.push(replaced);
    }

    drop(data);
    drop(displaced);

    let mut raw = alloc::vec![0u8; endow.len()];
    endow.read_at(0, &mut raw);
    Ok(PendingHandles::Moving { table: handles, endow: raw, labels: labels.to_vec() })
}
