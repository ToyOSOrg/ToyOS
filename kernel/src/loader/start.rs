//! Loads a built process onto a CPU, builds the handle table it starts with
//! and puts its spawner's handle to it in the spawner's table. The frame a new
//! stack starts from and the trampolines it returns into are the
//! architecture's (`arch::entry`), and [`Start`] is the only way to one: a
//! trampoline that returns to userland is reached with an
//! [`Entry`](toyos_userbound::Entry) and with nothing else.

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::object::job::JobObject;
use crate::object::notice::NoticeObject;
use crate::object::process::ProcessObject;
use crate::object::{ops, HandleEntry, HandleTable, KObjectRef, Refusal};
use crate::process::{
    process_data, Endowments, OwnedAlloc, ENDOW_ENTRY_LEN, KERNEL_STACK_SIZE,
};
use crate::scheduler;
use crate::user_ptr::UserBytes;
use toyos_abi::handle::{RawHandle, Rights};
use kernel::sched::job::Job;
use toyos_abi::syscall::{
    EndowEntry, SyscallError, JOB_LABEL, MAX_SPAWN_ENDOWMENTS, MAX_SPAWN_LABELS_LEN, QUIT_LABEL, SELF_LABEL,
};

/// Where a new context first runs.
pub(crate) enum Start {
    /// A program's first instruction, on the stack its loader wrote.
    Process { entry: toyos_userbound::Entry, sp: u64 },
    /// A thread's, on a stack its process chose, with its argument.
    Thread { entry: toyos_userbound::Entry, sp: u64, arg: u64 },
    /// A kernel thread's body, which never returns to userland.
    Kernel { body: extern "C" fn(u64) -> !, arg: u64 },
}

/// A task's kernel stack, which only [`alloc_kernel_stack`] builds. A thread
/// of a process has above its top the area its user machine state is kept in
/// while it is switched out (`arch::fpu`), and a kernel thread has none: a
/// switch's save above [`top`](Self::top) lands inside this allocation.
pub(crate) struct KernelStack {
    alloc: OwnedAlloc,
    user: bool,
}

impl KernelStack {
    /// What one is allocated as, with the area or without it.
    fn layout_for(user: bool) -> core::alloc::Layout {
        let area = if user { crate::arch::fpu::area_bytes() } else { 0 };
        core::alloc::Layout::from_size_align(KERNEL_STACK_SIZE + area, crate::mm::PAGE_BYTES)
            .expect("a kernel stack's layout")
    }

    /// The stack's lowest byte, where its canary sits.
    pub(crate) fn bottom(&self) -> *mut u8 {
        self.alloc.ptr()
    }

    /// The stack's end, and where its area starts.
    pub(crate) fn top(&self) -> u64 {
        self.alloc.ptr() as u64 + KERNEL_STACK_SIZE as u64
    }

    /// Whether its task is a thread of a process, which has the area.
    pub(crate) fn is_user(&self) -> bool {
        self.user
    }

    /// The layout it was allocated with.
    #[cfg(feature = "heap-tripwire")]
    pub(crate) fn layout(&self) -> core::alloc::Layout {
        Self::layout_for(self.user)
    }
}

/// Allocate a kernel stack and lay out the frame `context_switch` will restore.
pub(crate) fn alloc_kernel_stack(start: Start) -> Option<(KernelStack, u64)> {
    use crate::arch::entry::{kernel_start, process_start, thread_start};
    let (trampoline, user_entry, user_sp, arg, user): (unsafe extern "C" fn(), _, _, _, _) = match start {
        Start::Process { entry, sp } => (process_start, entry.addr(), sp, 0, true),
        Start::Thread { entry, sp, arg } => (thread_start, entry.addr(), sp, arg, true),
        Start::Kernel { body, arg } => (kernel_start, body as usize as u64, 0, arg, false),
    };
    let layout = KernelStack::layout_for(user);
    let alloc = OwnedAlloc::new(layout.size(), layout.align())?;
    scheduler::write_stack_canary(&alloc);
    let stack = KernelStack { alloc, user };
    // SAFETY: `stack` is fresh and exclusively owned, and its `top` ends the stack.
    let frame = unsafe { crate::arch::entry::initial_frame(stack.top(), trampoline, user_entry, user_sp, arg) };
    Some((stack, frame))
}

/// The last path component, truncated to what a process entry can hold.
pub(crate) fn make_name(path: &str) -> [u8; crate::process::THREAD_NAME_LEN] {
    let filename = path.rsplit('/').next().unwrap_or(path);
    let mut name = [0u8; crate::process::THREAD_NAME_LEN];
    let len = filename.len().min(crate::process::THREAD_NAME_LEN - 1);
    name[..len].copy_from_slice(&filename.as_bytes()[..len]);
    name
}

/// A caller's request for a child's table: `endow` has not left the caller's table yet.
// The move must be last: an earlier move leaves a failed spawn's caller holding handles that name nothing.
pub struct PendingHandles {
    table: HandleTable,
    endow: Vec<u8>,
    labels: Vec<u8>,
}

impl PendingHandles {
    /// Take the endowed handles out of the caller's table and put its handle to the child in it, all under one lock hold: a refusal leaves the table unchanged.
    /// `own` is what the kernel puts in the child's table beside the endowments. The child's job is the one endowed under
    /// [`JOB_LABEL`], or else the caller's; the caller's handle to the child is the last answer.
    pub fn commit(self, own: Own) -> Result<(HandleTable, Endowments, Arc<Job>, RawHandle), Refusal> {
        let Self { mut table, endow, mut labels } = self;
        let data_arc = process_data();
        let mut data = data_arc.lock();

        let count = endow.len() / ENDOW_ENTRY_LEN;
        let mut moving: Vec<(EndowEntry, RawHandle)> = Vec::with_capacity(count);
        let mut endowed_job: Option<Arc<Job>> = None;
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
            let label = &labels[label_off as usize..end];
            // The kernel's own, which a caller's of that name would shadow in the child's lookup.
            if Own::LABELS.iter().any(|own| label == own.as_bytes()) {
                return Err(SyscallError::InvalidArgument.into());
            }
            if label == JOB_LABEL.as_bytes() {
                // One owner: a second would be a job the child's lookup cannot name.
                if endowed_job.is_some() {
                    return Err(SyscallError::InvalidArgument.into());
                }
                endowed_job = Some(data.handles.get_sent::<JobObject>(handle, Rights::WRITE)?.job().clone());
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
        if !table.has_room(moving.len() + Own::LABELS.len()) || !data.handles.has_room(1) {
            return Err(SyscallError::ResourceExhausted.into());
        }

        let mut entries = Vec::with_capacity(moving.len());
        // The consoles this spawn endowed, by the parent's object, each with the child's own.
        let mut minted: Vec<(usize, KObjectRef)> = Vec::new();
        // Moved-out console handles, dropped once the parent's lock is let go.
        let mut displaced = Vec::new();
        for (mut entry, parent_handle) in moving {
            let moved = data
                .handles
                .remove(parent_handle)
                .expect("an endowed handle verified under this lock stopped resolving");
            // A console is minted, not moved: it is its own line buffer, and one shared
            // would splice two processes' half-lines. Two endowments of one parent console
            // are one child console, or aliased stdout and stderr would split in two.
            let moved = match moved.object() {
                KObjectRef::Console(parent_console) => {
                    let key = Arc::as_ptr(parent_console) as usize;
                    let object = match minted.iter().find(|(seen, _)| *seen == key) {
                        Some((_, object)) => object.clone(),
                        None => {
                            let object = KObjectRef::Console(crate::object::device::ConsoleObject::new());
                            minted.push((key, object.clone()));
                            object
                        }
                    };
                    let rights = moved.rights();
                    displaced.push(moved);
                    HandleEntry::new(object, rights)
                }
                _ => moved,
            };
            entry.handle = table
                .install(moved)
                .expect("a child table with verified room refused an endowment");
            entries.push(entry);
        }
        // Minted while `own` is held, and `own` goes into a table no thread reaches until the child lands: another thread of the caller closing this handle never closes the object's last.
        let held = ops::install(&mut data.handles, own.process.object().clone())
            .expect("a caller's table with verified room refused its child");
        let job = endowed_job.unwrap_or_else(|| data.job.clone());
        drop(data);
        drop(displaced);
        own.endow(&mut table, &mut entries, &mut labels);
        Ok((table, Endowments::new(entries, labels), job, held))
    }
}

/// What the kernel puts in every new process's table beside its endowments:
/// its handle to itself under [`SELF_LABEL`], the first its object has, and
/// its notice under [`QUIT_LABEL`], the only one its object ever has.
pub struct Own {
    process: HandleEntry,
    notice: HandleEntry,
}

impl Own {
    /// The labels [`Own::endow`] installs under, which no caller's endowment may use.
    const LABELS: [&str; 2] = [SELF_LABEL, QUIT_LABEL];

    /// `WRITE` to be named a spawn's place, `DUP` and `TRANSFER` to hand that on.
    pub(super) fn new(object: &Arc<ProcessObject>, notice: &Arc<NoticeObject>) -> Self {
        let rights = Rights::WRITE.union(Rights::DUP).union(Rights::TRANSFER);
        Self {
            process: HandleEntry::new(KObjectRef::Process(Arc::clone(object)), rights),
            notice: HandleEntry::new(KObjectRef::Notice(Arc::clone(notice)), NoticeObject::RIGHTS),
        }
    }

    /// Install both under their labels. The caller verified the room.
    pub(super) fn endow(self, table: &mut HandleTable, entries: &mut Vec<EndowEntry>, labels: &mut Vec<u8>) {
        for (entry, label) in [self.process, self.notice].into_iter().zip(Self::LABELS) {
            let handle = table.install(entry).expect("a child table with verified room refused the kernel's own handles");
            entries.push(EndowEntry { label_off: labels.len() as u32, label_len: label.len() as u32, handle, _pad: 0 });
            labels.extend_from_slice(label.as_bytes());
        }
    }
}

/// Reads `SpawnArgs`'s endowments into the child's pending handle state.
// Every refusal here precedes any child state, so nothing is enqueued or orphaned.
pub fn build_child_handles(endow: &UserBytes, labels: &[u8]) -> Result<PendingHandles, Refusal> {
    if endow.len() / ENDOW_ENTRY_LEN > MAX_SPAWN_ENDOWMENTS {
        return Err(SyscallError::InvalidArgument.into());
    }
    if labels.len() > MAX_SPAWN_LABELS_LEN {
        return Err(SyscallError::InvalidArgument.into());
    }
    let mut raw = alloc::vec![0u8; endow.len()];
    endow.read_at(0, &mut raw);
    Ok(PendingHandles { table: HandleTable::new(), endow: raw, labels: labels.to_vec() })
}
