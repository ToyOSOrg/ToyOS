//! Loads a built process onto a CPU, builds the handle table it starts with
//! and puts its spawner's handle to it in the spawner's table. The frame a new
//! stack starts from and the trampolines it returns into are the
//! architecture's (`arch::entry`), and [`Start`] is the only way to one: a
//! trampoline that returns to userland is reached with an
//! [`Entry`](toyos_userbound::Entry) and with nothing else.

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::mm::charge::{self, Charge};
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
    /// The stack, and the records its task keeps for as long as it does.
    _charge: Charge,
}

impl KernelStack {
    /// The heap a thread takes: its stack, its scheduler records, its place
    /// on the one watch it may wait on, whose list doubles, and, for a thread
    /// of a process, its place in the process's thread table, its own data,
    /// and what one of its calls copies in and may hold while it is preempted
    /// — two strings at their bound, as a rename's paths, and a page besides,
    /// as a namespace's names. A spawn charges what it copies past that.
    fn heap(user: bool) -> usize {
        let waiter = 2 * core::mem::size_of::<(usize, u64)>();
        let call = 2 * crate::user_ptr::MAX_USER_STR as usize + crate::mm::PAGE_BYTES;
        let records = kernel::sched::task::TaskBuilder::<crate::sched::payload::KernelPayload>::allocations().into_iter().map(charge::heap).sum::<usize>()
            + charge::arc::<crate::sched::payload::TaskHandle>()
            + waiter;
        let process = charge::map_entry::<toyos_abi::Tid, crate::process::ThreadEntry>()
            + charge::arc::<crate::sync::Lock<crate::process::ThreadData>>()
            + call;
        charge::heap(Self::layout_for(user)) + records + if user { process } else { 0 }
    }

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

/// Allocate a kernel stack, charged to the job its task runs in, and lay out
/// the frame `context_switch` will restore.
pub(crate) fn alloc_kernel_stack(job: &Arc<Job>, start: Start) -> Option<(KernelStack, u64)> {
    use crate::arch::entry::{kernel_start, process_start, thread_start};
    let (trampoline, user_entry, user_sp, arg, user): (unsafe extern "C" fn(), _, _, _, _) = match start {
        Start::Process { entry, sp } => (process_start, entry.addr(), sp, 0, true),
        Start::Thread { entry, sp, arg } => (thread_start, entry.addr(), sp, arg, true),
        Start::Kernel { body, arg } => (kernel_start, body as usize as u64, 0, arg, false),
    };
    let charge = Charge::new(job, KernelStack::heap(user)).ok()?;
    let layout = KernelStack::layout_for(user);
    let alloc = OwnedAlloc::new(layout.size(), layout.align())?;
    scheduler::write_stack_canary(&alloc);
    let stack = KernelStack { alloc, user, _charge: charge };
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
    /// The job the child runs in, which everything built for it is counted to.
    job: Arc<Job>,
    /// `SpawnArgs::pager` named a `SysCap` carrying `Rights::PAGER`.
    pager: bool,
}

/// What a spawn's caller gives the child: the job it runs in, and its handle
/// table, made once nothing is left to refuse.
pub trait Commit {
    /// What the caller is left holding of the child.
    type Held;

    /// The job the child runs in, which everything built for it is counted to.
    fn job(&self) -> &Arc<Job>;

    /// Whether the child starts as a pager (`process::PagerRole`).
    fn pager(&self) -> bool;

    /// The child's table around `own`, what the kernel puts beside its
    /// endowments.
    fn commit(self, own: Own) -> Result<(HandleTable, Endowments, Self::Held), Refusal>;
}

impl Commit for PendingHandles {
    type Held = RawHandle;

    fn job(&self) -> &Arc<Job> {
        &self.job
    }

    fn pager(&self) -> bool {
        self.pager
    }

    /// Take the endowed handles out of the caller's table and put its handle to the child in it, all under one lock hold: a refusal leaves the table unchanged.
    /// `own` is what the kernel puts in the child's table beside the endowments; the caller's handle to the child is the
    /// last answer. The handle under [`JOB_LABEL`] still names [`job`](Self::job), or the spawn is refused.
    fn commit(self, own: Own) -> Result<(HandleTable, Endowments, RawHandle), Refusal> {
        let Self { mut table, endow, mut labels, job, .. } = self;
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
        // Before any removal: the table changed under the spawn if the job
        // everything was built for is no longer the one it names.
        if !Arc::ptr_eq(&job, endowed_job.as_ref().unwrap_or(&data.job)) {
            return Err(SyscallError::InvalidArgument.into());
        }
        // Checked before any removal, so a failed install can't strand a handle out of a table that never spawned.
        if !table.make_room(moving.len() + Own::LABELS.len()) || !data.handles.make_room(1) {
            return Err(SyscallError::ResourceExhausted.into());
        }
        // The consoles this spawn endowed, by the parent's object, each with
        // the child's own: minted before any removal too, since one can be
        // refused. A console is minted, not moved: it is its own line buffer,
        // and one shared would splice two processes' half-lines. Two
        // endowments of one parent console are one child console, or aliased
        // stdout and stderr would split in two.
        let mut minted: Vec<(usize, KObjectRef)> = Vec::new();
        for (_, handle) in &moving {
            if let KObjectRef::Console(parent_console) = data.handles.get_ref(*handle, Rights::NONE)? {
                let key = Arc::as_ptr(parent_console) as usize;
                if !minted.iter().any(|(seen, _)| *seen == key) {
                    let console = crate::object::device::ConsoleObject::new(&job).map_err(SyscallError::from)?;
                    minted.push((key, KObjectRef::Console(console)));
                }
            }
        }

        let mut entries = Vec::with_capacity(moving.len());
        // Moved-out console handles, dropped once the parent's lock is let go.
        let mut displaced = Vec::new();
        // Device claims moving into the child, whose mappings in the caller
        // are taken back once the caller's lock is let go.
        let mut claims = Vec::new();
        for (mut entry, parent_handle) in moving {
            let moved = data
                .handles
                .remove(parent_handle)
                .expect("an endowed handle verified under this lock stopped resolving");
            let moved = match moved.object() {
                KObjectRef::Console(parent_console) => {
                    let key = Arc::as_ptr(parent_console) as usize;
                    let (_, object) = minted.iter().find(|(seen, _)| *seen == key).expect("each endowed console was minted");
                    let object = object.clone();
                    let rights = moved.rights();
                    displaced.push(moved);
                    HandleEntry::new(object, rights)
                }
                KObjectRef::Device(claim) => {
                    claims.push(claim.clone());
                    moved
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
        drop(data);
        drop(displaced);
        for claim in claims {
            claim.revoke_mappings();
        }
        own.endow(&mut table, &mut entries, &mut labels);
        Ok((table, Endowments::new(entries, labels), held))
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
pub fn build_child_handles(endow: &UserBytes, labels: &[u8], pager: bool) -> Result<PendingHandles, Refusal> {
    if endow.len() / ENDOW_ENTRY_LEN > MAX_SPAWN_ENDOWMENTS {
        return Err(SyscallError::InvalidArgument.into());
    }
    if labels.len() > MAX_SPAWN_LABELS_LEN {
        return Err(SyscallError::InvalidArgument.into());
    }
    let mut raw = alloc::vec![0u8; endow.len()];
    endow.read_at(0, &mut raw);
    let job = child_job(&raw, labels)?;
    Ok(PendingHandles { table: HandleTable::new(&job), endow: raw, labels: labels.to_vec(), job, pager })
}

/// The job a child runs in: the one endowed under [`JOB_LABEL`], or else the
/// caller's. Only looked up: [`PendingHandles::commit`] refuses the rest.
fn child_job(raw: &[u8], labels: &[u8]) -> Result<Arc<Job>, Refusal> {
    let data_arc = process_data();
    let data = data_arc.lock();
    for entry in raw.as_chunks::<ENDOW_ENTRY_LEN>().0 {
        let word = |at: usize| u32::from_ne_bytes([entry[at], entry[at + 1], entry[at + 2], entry[at + 3]]) as usize;
        let named = word(0).checked_add(word(4)).and_then(|end| labels.get(word(0)..end));
        if named == Some(JOB_LABEL.as_bytes()) {
            return Ok(data.handles.get_sent::<JobObject>(RawHandle(word(8) as u32), Rights::WRITE)?.job().clone());
        }
    }
    Ok(data.job.clone())
}
