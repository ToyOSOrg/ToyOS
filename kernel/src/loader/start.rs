//! Loads a built process onto a CPU, builds the handle table it starts with
//! and puts its spawner's handle to it in the spawner's table. The frame a new
//! stack starts from and the trampolines it returns into are the
//! architecture's (`arch::entry`), and [`Start`] is the only way to one: a
//! trampoline that returns to userland is reached with an
//! [`Entry`](toyos_userbound::Entry) and with nothing else.

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::mm::charge::{self, Charge};
use crate::object::notice::NoticeObject;
use crate::object::process::ProcessObject;
use crate::object::{ops, HandleEntry, HandleTable, KObjectRef, Refusal};
use crate::process::{handles, KERNEL_STACK_SIZE};
use crate::user_ptr::UserBytes;
use toyos_abi::handle::{RawHandle, Rights, TransferEntry};
use kernel::sched::job::Job;
use toyos_abi::syscall::{SyscallError, MAX_SPAWN_ENDOWMENTS};

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
    /// process's thread table, its own data, and what a spawn it makes copies
    /// in and holds where it may be preempted: its path, name and build-id at
    /// their bounds, its ranges, its endowments, and what the device claims it
    /// moves handed out.
    fn heap() -> usize {
        use toyos_abi::syscall::{MAX_BUILD_ID, MAX_PATH, MAX_PROCESS_NAME, MAX_SPAWN_MAPS};
        let waiter = 2 * core::mem::size_of::<(usize, u64)>();
        let spawn = [MAX_PATH, MAX_PROCESS_NAME, MAX_BUILD_ID].into_iter().map(charge::array::<u8>).sum::<usize>()
            + charge::array::<super::Map>(MAX_SPAWN_MAPS)
            + charge::array::<TransferEntry>(MAX_SPAWN_ENDOWMENTS)
            + charge::array::<crate::pcidev::Handed>(MAX_SPAWN_ENDOWMENTS);
        let records = kernel::sched::task::TaskBuilder::<crate::sched::payload::KernelPayload>::allocations().into_iter().map(charge::heap).sum::<usize>()
            + charge::arc::<crate::sched::payload::TaskHandle>()
            + waiter;
        let process = charge::map_entry::<toyos_abi::Tid, crate::process::ThreadEntry>()
            + charge::arc::<crate::sync::Lock<crate::process::ThreadData>>()
            + spawn;
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
    endow: Vec<TransferEntry>,
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

    /// The child's table: `own`, then what the caller gives it, in the
    /// order `RawHandle::endowed` states.
    fn commit(self, own: Own) -> Result<(HandleTable, Self::Held), Refusal>;
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
    fn commit(self, own: Own) -> Result<(HandleTable, RawHandle), Refusal> {
        let Self { mut table, endow, .. } = self;
        let caller = handles();
        let mut handles = caller.lock();

        let mut moving: Vec<(RawHandle, Rights)> = Vec::with_capacity(endow.len());
        for moved in endow {
            let want = moved.rights().map_err(|_| SyscallError::InvalidArgument)?;
            // Checked before any removal, so a missing `TRANSFER` refuses the spawn instead of leaving a hole.
            let rights = handles.rights_to_move(moved.handle, want)?;
            // A duplicate is refused here: unremoved handles let it pass twice, then the second removal's
            // `expect` below panics.
            if moving.iter().any(|(seen, _)| *seen == moved.handle) {
                return Err(SyscallError::InvalidArgument.into());
            }
            moving.push((moved.handle, rights));
        }
        // Checked before any removal, so a failed install can't strand a handle out of a table that never spawned.
        if !table.make_room(Own::HANDLES + moving.len()) || !handles.make_room(1) {
            return Err(SyscallError::ResourceExhausted.into());
        }
        // Minted while the child's table holds its own, a table no thread reaches until the child lands: another thread of the caller closing this handle never closes the object's last.
        let child = own.process.object().clone();
        own.install(&mut table);
        // What each device claim moving into the child handed out, taken off
        // it under the caller's lock and revoked once that lock is let go:
        // sized at the first for every handle left to move, so it never
        // grows past what `KernelStack::heap` charges.
        let mut handed = Vec::new();
        let count = moving.len();
        for (i, (parent_handle, rights)) in moving.into_iter().enumerate() {
            let moved = handles
                .remove(parent_handle)
                .expect("an endowed handle verified under this lock stopped resolving")
                .arriving_with(rights);
            if let KObjectRef::Device(claim) = moved.object() {
                if let Some(mappings) = claim.take_mappings() {
                    handed.reserve_exact(count - i);
                    handed.push(mappings);
                }
            }
            let installed = table.install(moved).expect("a child table with verified room refused an endowment");
            assert_eq!(installed, RawHandle::endowed(i), "a new table issued its handles out of order");
        }
        let held = ops::install(&mut handles, child).expect("a caller's table with verified room refused its child");
        drop(handles);
        for handed in handed {
            handed.revoke();
        }
        Ok((table, held))
    }
}

/// What the kernel puts first in every new process's table: its handle to
/// itself at [`RawHandle::SELF`], the first its object has, and its notice
/// at [`RawHandle::NOTICE`], the only one its object ever has.
pub struct Own {
    process: HandleEntry,
    notice: HandleEntry,
}

impl Own {
    /// How many handles [`Own::install`] installs.
    const HANDLES: usize = 2;

    /// `WRITE` to be named a spawn's place, `DUP` and `TRANSFER` to hand that on.
    pub(super) fn new(object: &Arc<ProcessObject>, notice: &Arc<NoticeObject>) -> Self {
        let rights = Rights::WRITE.union(Rights::DUP).union(Rights::TRANSFER);
        Self {
            process: HandleEntry::new(KObjectRef::Process(Arc::clone(object)), rights),
            notice: HandleEntry::new(KObjectRef::Notice(Arc::clone(notice)), NoticeObject::RIGHTS),
        }
    }

    /// Install both in `table`, which holds nothing yet. The caller verified the room.
    pub(super) fn install(self, table: &mut HandleTable) {
        for (entry, at) in [(self.process, RawHandle::SELF), (self.notice, RawHandle::NOTICE)] {
            let handle = table.install(entry).expect("a child table with verified room refused the kernel's own handles");
            assert_eq!(handle, at, "a new table issued its handles out of order");
        }
    }
}

/// Reads `SpawnArgs`'s endowments into the child's pending handle state, in the job `job`.
// Every refusal here precedes any child state, so nothing is enqueued or orphaned.
pub fn build_child_handles(endow: &UserBytes, job: Arc<Job>, pager: bool) -> Result<PendingHandles, Refusal> {
    let count = endow.len() / TRANSFER_ENTRY_LEN;
    if count > MAX_SPAWN_ENDOWMENTS {
        return Err(SyscallError::InvalidArgument.into());
    }
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let mut entry: TransferEntry = toyos_abi::usersafe::zeroed();
        endow.read_at(i * TRANSFER_ENTRY_LEN, toyos_abi::usersafe::bytes_mut(&mut entry)).map_err(SyscallError::from)?;
        entries.push(entry);
    }
    Ok(PendingHandles { table: HandleTable::new(&job), endow: entries, job, pager })
}

/// One endowment of a spawn's request on the wire.
const TRANSFER_ENTRY_LEN: usize = core::mem::size_of::<TransferEntry>();
