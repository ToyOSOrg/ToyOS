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
    handles, process_data, with_handles, Endowments, KERNEL_STACK_SIZE,
};
use crate::user_ptr::UserBytes;
use toyos_abi::handle::{RawHandle, Rights};
use kernel::sched::job::Job;
use toyos_abi::syscall::{
    EndowEntry, SpawnEndow, SyscallError, JOB_LABEL, MAX_SPAWN_ENDOWMENTS, MAX_SPAWN_LABELS_LEN, QUIT_LABEL, SELF_LABEL,
};

/// Where a new context first runs.
pub(crate) enum Start {
    /// A program's first instruction, on its empty stack.
    Process { entry: toyos_userbound::Entry, sp: u64 },
    /// A thread's, on a stack its process chose, with its argument.
    Thread { entry: toyos_userbound::Entry, sp: u64, arg: u64 },
}

/// A task's kernel stack, which only [`alloc_kernel_stack`] builds, and above
/// its top the area its thread's user machine state is kept in while it is
/// switched out (`arch::fpu`): a switch's save above [`top`](Self::top) lands
/// inside this allocation.
pub(crate) struct KernelStack {
    stack: crate::mm::stack::Stack,
    /// The stack, and the records its task keeps for as long as it does.
    _charge: Charge,
}

impl KernelStack {
    /// The heap a thread takes beside its stack's frames: its scheduler records, its place
    /// on the one watch it may wait on, whose list doubles, its place in its
    /// process's thread table, its own data, and what one of its calls copies
    /// in and may hold while it is preempted — two strings at their bound, as
    /// a rename's paths, and a page besides, as a namespace's names.
    fn heap() -> usize {
        let waiter = 2 * core::mem::size_of::<(usize, u64)>();
        let call = 2 * crate::user_ptr::MAX_USER_STR as usize + crate::mm::PAGE_BYTES;
        let records = kernel::sched::task::TaskBuilder::<crate::sched::payload::KernelPayload>::allocations().into_iter().map(charge::heap).sum::<usize>()
            + charge::arc::<crate::sched::payload::TaskHandle>()
            + waiter;
        let process = charge::map_entry::<toyos_abi::Tid, crate::process::ThreadEntry>()
            + charge::arc::<crate::sync::Lock<crate::process::ThreadData>>()
            + call;
        records + process
    }

    /// The stack and the area above it, in whole pages.
    fn bytes() -> usize {
        KERNEL_STACK_SIZE + crate::arch::fpu::area_bytes().next_multiple_of(crate::mm::PAGE_BYTES)
    }

    /// The stack's lowest byte, above its guard page.
    #[cfg(any(feature = "stack-witness", feature = "heap-tripwire"))]
    pub(crate) fn bottom(&self) -> *mut u8 {
        self.stack.bottom()
    }

    /// The stack's end, and where its area starts.
    pub(crate) fn top(&self) -> u64 {
        self.stack.bottom() as u64 + KERNEL_STACK_SIZE as u64
    }
}

/// Allocate a kernel stack, its frames held by the job its task runs in, and lay out
/// the frame `context_switch` will restore.
pub(crate) fn alloc_kernel_stack(job: &Arc<Job>, start: Start) -> Option<(KernelStack, u64)> {
    use crate::arch::entry::{process_start, thread_start};
    let (trampoline, user_entry, user_sp, arg): (unsafe extern "C" fn(), _, _, _) = match start {
        Start::Process { entry, sp } => (process_start, entry.addr(), sp, 0),
        Start::Thread { entry, sp, arg } => (thread_start, entry.addr(), sp, arg),
    };
    let charge = Charge::new(job, KernelStack::heap()).ok()?;
    let stack = KernelStack { stack: crate::mm::stack::Stack::new(job, KernelStack::bytes())?, _charge: charge };
    #[cfg(feature = "heap-tripwire")]
    crate::scheduler::paint_stack(&stack);
    // SAFETY: `stack` is fresh and exclusively owned, and its `top` ends the stack.
    let frame = unsafe { crate::arch::entry::initial_frame(stack.top(), trampoline, user_entry, user_sp, arg) };
    Some((stack, frame))
}

/// A caller's request for a child's table: `endow` has not left the caller's table yet.
// The move must be last: an earlier move leaves a failed spawn's caller holding handles that name nothing.
pub struct PendingHandles {
    table: HandleTable,
    endow: Vec<SpawnEndow>,
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

    /// Whether the child starts as a pager (`process::Handles::is_pager`).
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
        let own_job = process_data().lock().job.clone();
        let caller = handles();
        let mut handles = caller.lock();

        let mut moving: Vec<(EndowEntry, RawHandle, Rights)> = Vec::with_capacity(endow.len());
        let mut endowed_job: Option<Arc<Job>> = None;
        for SpawnEndow { label_off, label_len, moved } in endow {
            let handle = moved.handle;
            let want = moved.rights().map_err(|_| SyscallError::InvalidArgument)?;
            let label = label_of(&labels, label_off, label_len).ok_or(SyscallError::InvalidArgument)?;
            // The kernel's own, which a caller's of that name would shadow in the child's lookup.
            if Own::LABELS.iter().any(|own| label == own.as_bytes()) {
                return Err(SyscallError::InvalidArgument.into());
            }
            if label == JOB_LABEL.as_bytes() {
                // One owner: a second would be a job the child's lookup cannot name.
                if endowed_job.is_some() {
                    return Err(SyscallError::InvalidArgument.into());
                }
                endowed_job = Some(handles.get_sent::<JobObject>(handle, Rights::WRITE)?.job().clone());
            }
            // Checked before any removal, so a missing `TRANSFER` refuses the spawn instead of leaving a hole.
            let rights = handles.rights_to_move(handle, want)?;
            // A duplicate is refused here: unremoved handles let it pass twice, then the second removal's
            // `expect` below panics.
            if moving.iter().any(|(_, seen, _)| *seen == handle) {
                return Err(SyscallError::InvalidArgument.into());
            }
            moving.push((EndowEntry { label_off, label_len, handle, _pad: 0 }, handle, rights));
        }
        // Before any removal: the table changed under the spawn if the job
        // everything was built for is no longer the one it names.
        if !Arc::ptr_eq(&job, endowed_job.as_ref().unwrap_or(&own_job)) {
            return Err(SyscallError::InvalidArgument.into());
        }
        // Checked before any removal, so a failed install can't strand a handle out of a table that never spawned.
        if !table.make_room(moving.len() + Own::LABELS.len()) || !handles.make_room(1) {
            return Err(SyscallError::ResourceExhausted.into());
        }
        let mut entries = Vec::with_capacity(moving.len());
        // What each device claim moving into the child handed out, taken off
        // it under the caller's lock and revoked once that lock is let go.
        let mut handed = Vec::new();
        for (mut entry, parent_handle, rights) in moving {
            let moved = handles
                .remove(parent_handle)
                .expect("an endowed handle verified under this lock stopped resolving")
                .arriving_with(rights);
            if let KObjectRef::Device(claim) = moved.object() {
                handed.extend(claim.take_mappings());
            }
            entry.handle = table
                .install(moved)
                .expect("a child table with verified room refused an endowment");
            entries.push(entry);
        }
        // Minted while `own` is held, and `own` goes into a table no thread reaches until the child lands: another thread of the caller closing this handle never closes the object's last.
        let held = ops::install(&mut handles, own.process.object().clone())
            .expect("a caller's table with verified room refused its child");
        drop(handles);
        for handed in handed {
            handed.revoke();
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
    let count = endow.len() / SPAWN_ENDOW_LEN;
    if count > MAX_SPAWN_ENDOWMENTS {
        return Err(SyscallError::InvalidArgument.into());
    }
    if labels.len() > MAX_SPAWN_LABELS_LEN {
        return Err(SyscallError::InvalidArgument.into());
    }
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let mut entry: SpawnEndow = toyos_abi::usersafe::zeroed();
        endow.read_at(i * SPAWN_ENDOW_LEN, toyos_abi::usersafe::bytes_mut(&mut entry)).map_err(SyscallError::from)?;
        entries.push(entry);
    }
    let job = child_job(&entries, labels)?;
    Ok(PendingHandles { table: HandleTable::new(&job), endow: entries, labels: labels.to_vec(), job, pager })
}

/// One endowment of a spawn's request on the wire.
const SPAWN_ENDOW_LEN: usize = core::mem::size_of::<SpawnEndow>();

/// The label an entry's offset and length name in `labels`, if they are inside it.
fn label_of(labels: &[u8], off: u32, len: u32) -> Option<&[u8]> {
    labels.get(off as usize..(off as usize).checked_add(len as usize)?)
}

/// The job a child runs in: the one endowed under [`JOB_LABEL`], or else the
/// caller's. Only looked up: [`PendingHandles::commit`] refuses the rest.
fn child_job(endow: &[SpawnEndow], labels: &[u8]) -> Result<Arc<Job>, Refusal> {
    for entry in endow {
        if label_of(labels, entry.label_off, entry.label_len) == Some(JOB_LABEL.as_bytes()) {
            let named = with_handles(|handles| handles.get_sent::<JobObject>(entry.moved.handle, Rights::WRITE))?;
            return Ok(named.job().clone());
        }
    }
    Ok(process_data().lock().job.clone())
}
