use core::mem::{offset_of, size_of};
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicU8};

use alloc::alloc::alloc_zeroed;
use core::alloc::Layout;

use super::cpu;
use crate::log;

const MSR_GS_BASE: u32 = 0xC000_0101;

// GDT selectors (must match entry order)
pub const KERNEL_CS: u16 = 0x08;
pub const KERNEL_DS: u16 = 0x10;
/// The two selectors a thread runs userland with. RPL 3 is part of the value.
pub const USER_DS: u16 = 0x1B;
pub const USER_CS: u16 = 0x23;
const TSS_SEL: u16 = 0x28;

/// `STAR[63:48]`; SYSRET sets SS to this+8, CS to this+16, and RPL 3 must be baked in here — AMD doesn't OR it into SS as Intel does.
pub const STAR_SYSRET_BASE: u16 = USER_DS - 8;
const _: () = assert!(STAR_SYSRET_BASE + 8 == USER_DS);
const _: () = assert!(STAR_SYSRET_BASE + 16 == USER_CS);

/// 64-bit TSS (104 bytes).
#[repr(C, packed)]
pub struct Tss {
    reserved0: u32,
    pub rsp0: u64,
    rsp1: u64,
    rsp2: u64,
    reserved1: u64,
    ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    iopb_offset: u16,
}

impl Tss {
    const fn new() -> Self {
        Self {
            reserved0: 0,
            rsp0: 0,
            rsp1: 0,
            rsp2: 0,
            reserved1: 0,
            ist: [0; 7],
            reserved2: 0,
            reserved3: 0,
            iopb_offset: size_of::<Tss>() as u16,
        }
    }
}

/// Per-CPU fault state machine for the escalation policy on nested faults.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CpuFaultState {
    Normal = 0,
    PageFault = 1,   // demand paging in progress
    Fatal = 2,       // fatal exception handler running
    Panic = 3,       // panic handler running
}

/// Per-CPU data, reached through the GS segment; every access names an `OFF_*` constant below, so a field move carries its accessors.
#[repr(C)]
pub struct PerCpu {
    self_ptr: u64,
    cpu_id: u32,
    /// The syscall entry loads this as its kernel stack.
    pub kernel_rsp: u64,
    /// …and parks the user's RSP here across the switch.
    pub user_rsp: u64,
    pub tss: Tss,
    /// TID of the thread running on this CPU; `u32::MAX` when none is.
    current_tid: u32,
    /// PID of the process running on this CPU; `u32::MAX` when none is.
    current_pid: u32,
    gdt: [u64; 7],
    idle_stack_top: u64,
    /// Saved user RIP at last syscall entry (for panic diagnostics).
    pub syscall_rip: u64,
    /// Saved syscall number (for panic diagnostics).
    pub syscall_num: u64,
    /// Saved user RBP at last syscall entry (for panic diagnostics).
    pub syscall_rbp: u64,
    /// The task whose syscall this CPU is inside: packed pid:tid, or [`NO_SYSCALL`].
    syscall_task: u64,
    /// `lock add/sub`: IRQ entry/exit and kernel code both mutate it on this CPU.
    pub preempt_count: AtomicU32,
    pub need_resched: AtomicU8,
    _pad_after_need_resched: [u8; 3],
    /// Writes use plain `inc`: only the Ring 0 timer stub writes, with IF=0.
    pub ring0_timer_fires: AtomicU32,
    pub last_seen_ring0_fires: u32,
    fault_state: u8,
    _pad_after_fault_state: [u8; 3],
    /// Ticks the Ring 0 timer re-arms with; per-CPU to avoid cross-CPU clobber.
    pub last_armed_ticks: AtomicU32,
    /// This CPU's [`log::Shard`]; never null on a live CPU ([`alloc_percpu`] fills it first).
    log_shard: u64,
    /// Non-zero inside this CPU's NMI handler, written only by `arch::idt::nmi`'s entry; IST2 isn't re-entrant, so this proves no second NMI lands on it.
    nmi_active: u32,
    /// The token of the attempt that booted this AP; the AP echoes it into `AP_STARTED` so a stale AP cannot answer for a later attempt. Zero on the BSP.
    ap_token: u32,
    /// Interrupt deliveries, one counter per `irq_census::Source`; written only by `irq_census::irq_took!`, kept last so growing `SLOTS` moves nothing else.
    pub irq_counts: [AtomicU64; crate::irq_census::SLOTS],
}

const GDT_ENTRIES: [u64; 7] = [
    0x0000_0000_0000_0000, // null
    0x00AF_9A00_0000_FFFF, // kernel code64
    0x00CF_9200_0000_FFFF, // kernel data
    0x00CF_F200_0000_FFFF, // user data
    0x00AF_FA00_0000_FFFF, // user code64
    0,                      // TSS low (runtime)
    0,                      // TSS high (runtime)
];

#[repr(C, packed)]
struct GdtPointer {
    limit: u16,
    base: u64,
}

impl PerCpu {
    fn init_tss_descriptor(&mut self) {
        let tss_addr = &self.tss as *const Tss as u64;
        let tss_limit = (size_of::<Tss>() - 1) as u64;

        let low = (tss_limit & 0xFFFF)
            | ((tss_addr & 0xFFFF) << 16)
            | (((tss_addr >> 16) & 0xFF) << 32)
            | (0x89u64 << 40)
            | (((tss_limit >> 16) & 0xF) << 48)
            | (((tss_addr >> 24) & 0xFF) << 56);
        let high = tss_addr >> 32;

        self.gdt[5] = low;
        self.gdt[6] = high;
    }

    /// Load this CPU's GDT, reload segment registers, and load TSS.
    /// # Safety: must be called exactly once per CPU during init.
    unsafe fn load_gdt(&self) {
        let ptr = GdtPointer {
            limit: (size_of::<[u64; 7]>() - 1) as u16,
            base: self.gdt.as_ptr() as u64,
        };

        core::arch::asm!(
            "lgdt [{}]",
            "push {cs}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            "mov ds, {ds:x}",
            "mov es, {ds:x}",
            "mov fs, {ds:x}",
            // GS is skipped: reloading its selector would zero the IA32_GS_BASE-loaded base.
            "mov ss, {ds:x}",
            in(reg) &ptr,
            cs = in(reg) KERNEL_CS as u64,
            ds = in(reg) KERNEL_DS as u64,
            tmp = lateout(reg) _,
        );

        cpu::ltr(TSS_SEL);
    }
}

// Every GS access reaches its field through one of these `const`s; no
// displacement is duplicated.
const OFF_SELF_PTR: u32 = offset_of!(PerCpu, self_ptr) as u32;
const OFF_CPU_ID: u32 = offset_of!(PerCpu, cpu_id) as u32;
/// The kernel stack `arch::syscall`'s entry switches to.
pub(crate) const OFF_KERNEL_RSP: u32 = offset_of!(PerCpu, kernel_rsp) as u32;
pub(crate) const OFF_USER_RSP: u32 = offset_of!(PerCpu, user_rsp) as u32;
const OFF_CURRENT_TID: u32 = offset_of!(PerCpu, current_tid) as u32;
const OFF_CURRENT_PID: u32 = offset_of!(PerCpu, current_pid) as u32;
const OFF_IDLE_STACK_TOP: u32 = offset_of!(PerCpu, idle_stack_top) as u32;
pub(crate) const OFF_SYSCALL_RIP: u32 = offset_of!(PerCpu, syscall_rip) as u32;
pub(crate) const OFF_SYSCALL_NUM: u32 = offset_of!(PerCpu, syscall_num) as u32;
pub(crate) const OFF_SYSCALL_RBP: u32 = offset_of!(PerCpu, syscall_rbp) as u32;
const OFF_SYSCALL_TASK: u32 = offset_of!(PerCpu, syscall_task) as u32;
pub(crate) const OFF_RING0_TIMER_FIRES: u32 = offset_of!(PerCpu, ring0_timer_fires) as u32;
const OFF_LAST_SEEN_RING0_FIRES: u32 = offset_of!(PerCpu, last_seen_ring0_fires) as u32;
pub(crate) const OFF_LAST_ARMED_TICKS: u32 = offset_of!(PerCpu, last_armed_ticks) as u32;
/// Used by `reserve_log_slot`'s asm so no GS access spells a raw offset.
const OFF_LOG_SHARD: u32 = offset_of!(PerCpu, log_shard) as u32;
/// Opened/closed by `arch::syscall`'s and `arch::idt`'s naked stubs; `preempt` is the Rust half.
pub(crate) const OFF_PREEMPT_COUNT: u32 = offset_of!(PerCpu, preempt_count) as u32;
/// Set by the timer ISR, cleared by the deferred-preempt epilogue — `preempt`.
pub(crate) const OFF_NEED_RESCHED: u32 = offset_of!(PerCpu, need_resched) as u32;
/// Read by `preempt::enable`'s slow path to decline rescheduling a faulting CPU.
pub(crate) const OFF_FAULT_STATE: u32 = offset_of!(PerCpu, fault_state) as u32;
/// Set/cleared only by `arch::idt::nmi`'s naked entry, via this constant.
pub(crate) const OFF_NMI_ACTIVE: u32 = offset_of!(PerCpu, nmi_active) as u32;
/// The AP's bring-up token, read by `ap_entry` to answer for its own attempt.
const OFF_AP_TOKEN: u32 = offset_of!(PerCpu, ap_token) as u32;
/// Where this CPU's interrupt counters start; `irq_census::slot_offset` derives every handler's offset from it.
pub const OFF_IRQ_COUNTS: u32 = offset_of!(PerCpu, irq_counts) as u32;


/// Every GS-relative access this kernel makes, as `const`-generic primitives.
/// Caller-owed and unchecked: `GS_BASE` must already point at this CPU's `PerCpu`
/// — set by [`init_bsp`] on the BSP and by the AP trampoline before an AP runs Rust.
pub(crate) mod gs {
    use core::arch::asm;

    /// One naturally aligned per-CPU `u64` load.
    #[inline]
    pub fn read_u64<const OFF: u32>() -> u64 {
        let v: u64;
        // SAFETY: `OFF` is asserted; GS_BASE points at this CPU's PerCpu per the module contract.
        unsafe {
            asm!("mov {v}, gs:[{off}]", v = out(reg) v, off = const OFF,
                options(nomem, nostack, preserves_flags));
        }
        v
    }

    /// One naturally aligned per-CPU `u32` load.
    #[inline]
    pub fn read_u32<const OFF: u32>() -> u32 {
        let v: u32;
        // SAFETY: `read_u64`'s argument, for four bytes.
        unsafe {
            asm!("mov {v:e}, gs:[{off}]", v = out(reg) v, off = const OFF,
                options(nomem, nostack, preserves_flags));
        }
        v
    }

    /// One naturally aligned per-CPU `u32` store; no `lock` prefix — aligned stores are atomic on x86.
    #[inline]
    pub fn write_u32<const OFF: u32>(v: u32) {
        // SAFETY: `read_u64`'s argument, minus `nomem`; single-CPU-writer atomicity is the sync.
        unsafe {
            asm!("mov gs:[{off}], {v:e}", off = const OFF, v = in(reg) v,
                options(nostack, preserves_flags));
        }
    }

    /// One naturally aligned per-CPU `u64` store. No `lock` prefix, per [`write_u32`] at eight bytes.
    #[inline]
    pub fn write_u64<const OFF: u32>(v: u64) {
        // SAFETY: `write_u32`'s argument, for eight bytes.
        unsafe {
            asm!("mov gs:[{off}], {v}", off = const OFF, v = in(reg) v,
                options(nostack, preserves_flags));
        }
    }

    /// One per-CPU byte load.
    #[inline]
    pub fn read_u8<const OFF: u32>() -> u8 {
        let v: u8;
        // SAFETY: `read_u64`'s argument, for one byte.
        unsafe {
            asm!("mov {v}, gs:[{off}]", v = out(reg_byte) v, off = const OFF,
                options(nomem, nostack, preserves_flags));
        }
        v
    }

    /// One per-CPU byte store, from a register; single-byte stores are atomic on x86, no `lock` needed.
    #[inline]
    pub fn write_u8<const OFF: u32>(v: u8) {
        // SAFETY: `write_u32`'s argument, for one byte.
        unsafe {
            asm!("mov gs:[{off}], {v}", off = const OFF, v = in(reg_byte) v,
                options(nostack, preserves_flags));
        }
    }

    /// One per-CPU byte store from an *immediate* — avoids materialising the constant into a register first.
    #[inline]
    pub fn write_u8_imm<const OFF: u32, const VAL: u8>() {
        // SAFETY: `write_u32`'s argument, for one byte.
        unsafe {
            asm!("mov byte ptr gs:[{off}], {val}", off = const OFF, val = const VAL,
                options(nostack, preserves_flags));
        }
    }

    /// One `lock`-prefixed increment of a per-CPU `u32`; required since an IRQ can split a plain `add`'s load/store on this CPU.
    #[inline]
    pub fn lock_inc_u32<const OFF: u32>() {
        // SAFETY: `write_u32`'s argument; no `preserves_flags` — `lock add` writes OF/SF/ZF/AF/CF/PF.
        unsafe {
            asm!("lock add dword ptr gs:[{off}], 1", off = const OFF, options(nostack));
        }
    }

    /// One `lock`-prefixed decrement of a per-CPU `u32`. See [`lock_inc_u32`].
    #[inline]
    pub fn lock_dec_u32<const OFF: u32>() {
        // SAFETY: `lock_inc_u32`'s argument exactly.
        unsafe {
            asm!("lock sub dword ptr gs:[{off}], 1", off = const OFF, options(nostack));
        }
    }
}

/// Same size as a task's kernel stack: a `deferred` [`kobject!`] object may
/// own an `immediate` one, whose destructor then runs here instead.
const IDLE_STACK_SIZE: usize = crate::process::KERNEL_STACK_SIZE;

/// One unmapped 4 KiB page below every idle stack: unmapped, not filled like
/// [`IST_GUARD_SIZE`], so a fault here escalates to IST1's `#DF` rather than silently corrupting memory.
const IDLE_GUARD_SIZE: usize = 4096;

/// The IST stacks this machine has: IST1 `#DF`, IST2 NMI, IST3 `#MC` — vectors
/// that can arrive with `rsp` not a kernel stack (SDM Vol. 3A §6.14.5); `ist[n-1]` is IST*n*.
pub(crate) const IST_STACKS: usize = 3;

/// One size for every IST stack, for [`IDLE_STACK_SIZE`]'s reason; must leave room to double the measured high water, which `double_fault_stack` asserts.
const IST_STACK_SIZE: usize = 16384;

/// Filled with [`STACK_FILL`], not unmapped: a fault already on IST1 is a triple fault, so detecting after the fact beats trapping it.
const IST_GUARD_SIZE: usize = 4096;

/// Chosen so a zeroed or ASCII byte cannot be mistaken for untouched stack.
const STACK_FILL: u8 = 0xA5;
const STACK_FILL_WORD: u64 = u64::from_ne_bytes([STACK_FILL; 8]);

/// Allocate and initialize `PerCpu` for a CPU; the pointer lives forever, one `write` of the whole struct so a new field must be given a value here.
fn alloc_percpu(cpu_id: u32) -> *mut PerCpu {
    let layout = Layout::from_size_align(size_of::<PerCpu>(), 16).unwrap();
    // SAFETY: size non-zero, 16 a power of two; never freed — published into IA32_GS_BASE for the machine's life.
    let ptr = unsafe { alloc_zeroed(layout) } as *mut PerCpu;
    assert!(!ptr.is_null(), "percpu: alloc failed");

    // SAFETY: the allocation succeeded (asserted), is aligned and sized for one `PerCpu`, unreferenced so far.
    unsafe {
        core::ptr::write(
            ptr,
            PerCpu {
                self_ptr: ptr as u64,
                cpu_id,
                kernel_rsp: 0,
                user_rsp: 0,
                tss: Tss::new(),
                // The idle sentinel: zero would name thread 0 of process 0.
                current_tid: u32::MAX,
                current_pid: u32::MAX,
                gdt: GDT_ENTRIES,
                idle_stack_top: 0,
                syscall_rip: 0,
                syscall_num: 0,
                syscall_rbp: 0,
                syscall_task: NO_SYSCALL,
                preempt_count: AtomicU32::new(0),
                need_resched: AtomicU8::new(0),
                _pad_after_need_resched: [0; 3],
                ring0_timer_fires: AtomicU32::new(0),
                last_seen_ring0_fires: 0,
                fault_state: CpuFaultState::Normal as u8,
                _pad_after_fault_state: [0; 3],
                last_armed_ticks: AtomicU32::new(0),
                log_shard: alloc_log_shard(cpu_id),
                nmi_active: 0,
                ap_token: 0,
                irq_counts: [const { AtomicU64::new(0) }; crate::irq_census::SLOTS],
            },
        );
    }

    // SAFETY: the write above initialised it, and nothing else references it.
    let percpu = unsafe { &mut *ptr };
    // After the write, not inside it: the descriptor holds this block's own address.
    percpu.init_tss_descriptor();
    // Published before the CPU it belongs to runs an instruction — no window where the census misses it.
    crate::irq_census::publish(cpu_id, percpu.irq_counts.as_ptr());
    ptr
}

/// This CPU's log shard: cpu0's is the boot shard, every other fresh; allocated here rather than in [`init_ap`], which already logs.
fn alloc_log_shard(cpu_id: u32) -> u64 {
    if cpu_id == 0 {
        return &raw const log::BOOT_SHARD as u64;
    }
    let layout = Layout::from_size_align(size_of::<log::Shard>(), 64).unwrap();
    // SAFETY: `alloc_percpu`'s argument — non-zero size, power-of-two alignment, never freed.
    let ptr = unsafe { alloc_zeroed(layout) } as *mut log::Shard;
    assert!(!ptr.is_null(), "percpu: log shard alloc failed for cpu{cpu_id}");
    // SAFETY: fresh, zeroed, 64-byte-aligned, and not yet published.
    unsafe { log::Shard::initialize_zeroed(ptr) };
    // Published before the CPU executes an instruction: a reader can't find it through `gs:` otherwise.
    // SAFETY: the allocation is live for the machine's life and initialised.
    unsafe { log::publish_ap_shard(cpu_id, ptr) };
    ptr as u64
}

/// This CPU's shard, its identity, and one sequence number out of that shard.
/// The `xadd` has no `lock` prefix, sound only while the live [`crate::arch::LogCommitGuard`] proves ownership; the four reads are one `asm!` block, not four [`gs`] calls, so the absent `nomem` keeps shard selection inside the guard's barrier.
pub fn reserve_log_slot(
    guard: &crate::arch::LogCommitGuard,
) -> (*const log::Shard, u64, u32, u32, u32) {
    let shard: u64;
    let seq: u64;
    let cpu: u32;
    let tid: u32;
    let pid: u32;
    // SAFETY: `gs`'s contract — GS_BASE points at this CPU's `PerCpu`, proven unmigrated by the live `guard`.
    // `log_shard` is never null on a live CPU; `guard` passes to `reserve`, where the `xadd`'s soundness is stated.
    unsafe {
        core::arch::asm!(
            "mov {shard}, gs:[{shard_off}]",
            "mov {cpu:e}, gs:[{cpu_off}]",
            "mov {tid:e}, gs:[{tid_off}]",
            "mov {pid:e}, gs:[{pid_off}]",
            shard = out(reg) shard,
            cpu = out(reg) cpu,
            tid = out(reg) tid,
            pid = out(reg) pid,
            shard_off = const OFF_LOG_SHARD,
            cpu_off = const OFF_CPU_ID,
            tid_off = const OFF_CURRENT_TID,
            pid_off = const OFF_CURRENT_PID,
            options(preserves_flags),
        );
        // `log-nested-reserve`'s injection point: must sit between the shard-pointer read and the `xadd`, the only place ordering is decided (no-op outside tests).
        crate::log::nested::reserve_window();
        seq = (&*(shard as *const log::Shard)).reserve(guard);
    }
    (shard as *const log::Shard, seq, cpu, tid, pid)
}

/// One idle stack and the guard page under it.
const IDLE_SLOT: usize = IDLE_GUARD_SIZE + IDLE_STACK_SIZE;

/// Idle stacks come from their own 2 MiB pages, not the kernel heap: the guard's hole in the direct map would split a heap-shared leaf's TLB entry into 512.
/// Never freed — a leaf returned to the PMM would keep the hole.
static IDLE_STACKS: crate::sync::Lock<IdleArena> = crate::sync::Lock::new(IdleArena {
    pages: alloc::vec::Vec::new(),
    stacks: alloc::vec::Vec::new(),
    next: 0,
    left: 0,
});

struct IdleArena {
    pages: alloc::vec::Vec<crate::mm::pmm::PhysPage>,
    /// The bottom of every idle stack, so the deepest any CPU has gone reads from one.
    stacks: alloc::vec::Vec<u64>,
    /// Direct-map address of the next free slot.
    next: u64,
    left: usize,
}

/// A 4 KiB-aligned `IDLE_SLOT` from the arena.
fn alloc_idle_slot() -> u64 {
    let mut arena = IDLE_STACKS.lock();
    if arena.left < IDLE_SLOT {
        let page = crate::mm::pmm::alloc_page(crate::mm::pmm::Category::KernelHeap)
            .expect("percpu: no physical page for an idle stack");
        arena.next = page.direct_map().as_mut_ptr::<u8>() as u64;
        arena.left = crate::mm::PAGE_2M as usize;
        arena.pages.push(page);
    }
    let base = arena.next;
    arena.next += IDLE_SLOT as u64;
    arena.left -= IDLE_SLOT;
    arena.stacks.push(base + IDLE_GUARD_SIZE as u64);
    base
}

fn alloc_idle_stack(percpu: &mut PerCpu) {
    let base = alloc_idle_slot();
    crate::mm::paging::guard_kernel_page(base);
    // SAFETY: exactly `IDLE_STACK_SIZE` bytes above the unmapped guard, within the returned `IDLE_SLOT` — filled, not zeroed, so zero can't mark "untouched" for [`idle_stack_high_water`].
    unsafe {
        core::ptr::write_bytes(
            (base + IDLE_GUARD_SIZE as u64) as *mut u8,
            STACK_FILL,
            IDLE_STACK_SIZE,
        )
    };
    percpu.idle_stack_top = base + IDLE_SLOT as u64;
}

/// How big one idle stack is; read by `SYS_DEBUG` for scale.
#[cfg(feature = "test-actuators")]
pub fn idle_stack_size() -> usize {
    IDLE_STACK_SIZE
}

/// The deepest any CPU's idle stack has ever been, in bytes, read from the bottom up: nothing legitimate writes [`STACK_FILL`], so a touched byte stays changed.
#[cfg(feature = "test-actuators")]
pub fn idle_stack_high_water() -> usize {
    let arena = IDLE_STACKS.lock();
    arena
        .stacks
        .iter()
        .map(|&bottom| {
            let untouched =
                words(bottom, IDLE_STACK_SIZE).take_while(|&w| w == STACK_FILL_WORD).count() * 8;
            IDLE_STACK_SIZE - untouched
        })
        .max()
        .unwrap_or(0)
}

/// One stack per [`IST_STACKS`] row; an `ist[n-1]` left zero faults to address 0 unchecked.
fn alloc_ist_stacks(percpu: &mut PerCpu) {
    let total = IST_GUARD_SIZE + IST_STACK_SIZE;
    for slot in 0..IST_STACKS {
        let layout = Layout::from_size_align(total, 4096).unwrap();
        // SAFETY: `alloc_percpu`'s argument; 4096 is load-bearing — the guard starts at a page.
        let base = unsafe { alloc_zeroed(layout) };
        assert!(!base.is_null(), "percpu: IST{} stack alloc failed", slot + 1);
        // SAFETY: `total` bytes from `base`, exactly the allocation just made and asserted non-null.
        unsafe { core::ptr::write_bytes(base, STACK_FILL, total) };
        let top = base as u64 + total as u64;
        // SAFETY: `Tss` is `repr(C, packed)` (possibly unaligned); `slot < IST_STACKS <= 7 == Tss::ist.len()`.
        unsafe { core::ptr::write_unaligned(&raw mut percpu.tss.ist[slot], top); }
    }
}

const _: () = assert!(IST_STACKS <= 7, "a TSS has seven IST slots");

/// The IST1 stack top this CPU's TSS holds, if it looks like one — checked, not trusted, since callers are on the panic path where the block may be corrupt.
fn ist1_top() -> Option<u64> {
    let percpu = gs::read_u64::<OFF_SELF_PTR>() as *const PerCpu;
    if !crate::mm::is_kernel_addr(percpu as u64) {
        return None;
    }
    // SAFETY: the address is this CPU's own `self_ptr`, checked to be a kernel address; unaligned since `Tss` is packed.
    let top = unsafe { core::ptr::read_unaligned(&raw const (*percpu).tss.ist[0]) };
    let total = (IST_GUARD_SIZE + IST_STACK_SIZE) as u64;
    let base = top.checked_sub(total)?;
    (crate::mm::is_kernel_addr(base) && top % 4096 == 0).then_some(top)
}

/// Report how much of the double fault stack the crash report used, straight to the UART — bypassing the log ring, which is drained and may itself be corrupt.
pub fn ist1_report() {
    let Some(top) = ist1_top() else { return };
    let rsp = cpu::read_rsp();
    let stack_bottom = top - IST_STACK_SIZE as u64;
    if rsp < stack_bottom || rsp > top {
        return;
    }

    let guard_base = stack_bottom - IST_GUARD_SIZE as u64;
    let intact = words(guard_base, IST_GUARD_SIZE).all(|w| w == STACK_FILL_WORD);
    let untouched = words(stack_bottom, IST_STACK_SIZE)
        .take_while(|&w| w == STACK_FILL_WORD)
        .count()
        * 8;
    let used = IST_STACK_SIZE - untouched;

    crate::drivers::serial::panic_raw(b"\n[ist1] used ");
    crate::drivers::serial::panic_raw_dec(used as u64);
    crate::drivers::serial::panic_raw(b" of ");
    crate::drivers::serial::panic_raw_dec(IST_STACK_SIZE as u64);
    crate::drivers::serial::panic_raw(if intact {
        b" bytes, guard intact\n"
    } else {
        b" bytes, GUARD CORRUPTED\n"
    });
}

/// Sequential u64s from `base`; every address is inside the caller's already-bounds-checked allocation.
fn words(base: u64, len: usize) -> impl Iterator<Item = u64> {
    // SAFETY: `i < len/8` bounds each address inside the caller's checked allocation; `read_volatile` keeps the fill-pattern read.
    (0..len / 8).map(move |i| unsafe { core::ptr::read_volatile((base as *const u64).add(i)) })
}

/// Initialize per-CPU data for the BSP. Call after paging + allocator but before IDT/syscall.
pub fn init_bsp(lapic_id: u32) {
    let ptr = alloc_percpu(0);
    // SAFETY: `alloc_percpu` just returned a live, initialised `PerCpu` with no other reference until the `wrmsr` below.
    let percpu = unsafe { &mut *ptr };

    percpu.kernel_rsp = cpu::read_rsp();
    // SAFETY: `Tss` is `repr(C, packed)`; `rsp0` may be unaligned.
    unsafe { core::ptr::write_unaligned(&raw mut percpu.tss.rsp0, cpu::read_rsp()); }
    alloc_idle_stack(percpu);
    alloc_ist_stacks(percpu);

    // SAFETY: `load_gdt`'s once-per-CPU contract — this is the BSP's call; `init_ap` is every AP's.
    unsafe { percpu.load_gdt(); }
    super::control_regs::init(0);
    // A step with no record of its own is invisible on a machine whose only
    // channel is the panel: the last record painted is the whole of what a stop
    // says, so each step between `control_regs`' line and this function's own
    // gets one before it is taken.
    log!("percpu: cpu0 gdt loaded and control registers applied; the FPU's initial state is next");
    super::fpu::init();
    log!("percpu: cpu0 FPU initial state accepted; gs base and the per-CPU log path are next");

    // SAFETY: the write that makes `gs:` valid on the BSP; `ptr`'s `&mut` ended at `load_gdt` above, so this hands the CPU its only reference.
    unsafe { cpu::wrmsr(MSR_GS_BASE, ptr as u64) };

    // Ordering matters: gs: is invalid until the wrmsr above runs.
    crate::log::PERCPU_READY.store(true, core::sync::atomic::Ordering::Release);

    log!("percpu: BSP cpu_id=0 lapic_id={lapic_id}");
    super::fpu::log_state();
}

/// Allocate percpu for an AP; the trampoline writes the pointer into IA32_GS_BASE.
pub fn alloc_ap(cpu_id: u32, token: u32) -> *mut PerCpu {
    let ptr = alloc_percpu(cpu_id);
    // SAFETY: `init_bsp`'s argument; this AP hasn't been sent its INIT-SIPI yet, so it ran no instruction.
    let percpu = unsafe { &mut *ptr };
    percpu.ap_token = token;
    alloc_idle_stack(percpu);
    alloc_ist_stacks(percpu);
    ptr
}

/// Finish AP percpu init, called from `ap_entry` after the trampoline sets GS base.
pub fn init_ap(percpu_ptr: *mut PerCpu) {
    // SAFETY: this CPU's own `PerCpu`, read from `gs:[0]`; the BSP dropped its `&mut` before the SIPI.
    let percpu = unsafe { &mut *percpu_ptr };
    // SAFETY: `load_gdt`'s once-per-CPU contract; this is this AP's call.
    unsafe { percpu.load_gdt(); }
    super::control_regs::init(percpu.cpu_id);
    super::fpu::init();
    super::fpu::log_state();
}

/// Update `kernel_rsp` and `tss.rsp0` for a context switch to a new process.
/// # Safety: must be called from the CPU whose GS base points to the relevant PerCpu.
pub unsafe fn set_kernel_stack(rsp: u64) {
    let percpu = gs::read_u64::<OFF_SELF_PTR>() as *mut PerCpu;
    (*percpu).kernel_rsp = rsp;
    core::ptr::write_unaligned(&raw mut (*percpu).tss.rsp0, rsp);
}

/// The two words [`set_kernel_stack`] writes: `kernel_rsp` (syscall entry) and `tss.rsp0` (Ring 3 interrupt entry); read only by an instrument.
/// # Safety: must be called from the CPU whose GS base points to the relevant PerCpu.
#[cfg(feature = "stack-witness")]
pub unsafe fn entry_stacks() -> (u64, u64) {
    let percpu = gs::read_u64::<OFF_SELF_PTR>() as *const PerCpu;
    (
        (*percpu).kernel_rsp,
        core::ptr::read_unaligned(&raw const (*percpu).tss.rsp0),
    )
}

/// Read this CPU's ID from GS-relative percpu data.
pub fn cpu_id() -> u32 {
    gs::read_u32::<OFF_CPU_ID>()
}

pub fn ap_token() -> u32 {
    gs::read_u32::<OFF_AP_TOKEN>()
}

/// Read the Tid of the thread currently running on this CPU. None means idle.
pub fn current_tid() -> Option<crate::process::Tid> {
    let raw = gs::read_u32::<OFF_CURRENT_TID>();
    if raw == u32::MAX { None } else { Some(crate::process::Tid::from_raw(raw)) }
}

/// Set the Tid of the thread running on this CPU. None sets idle (u32::MAX).
pub fn set_current_tid(tid: Option<crate::process::Tid>) {
    gs::write_u32::<OFF_CURRENT_TID>(tid.map_or(u32::MAX, |t| t.raw()));
}

/// Read the Pid of the process running on this CPU. None means idle.
pub fn current_pid() -> Option<crate::process::Pid> {
    let raw = gs::read_u32::<OFF_CURRENT_PID>();
    if raw == u32::MAX { None } else { Some(crate::process::Pid::from_raw(raw)) }
}

/// Set the Pid of the process running on this CPU. None sets idle (u32::MAX).
pub fn set_current_pid(pid: Option<crate::process::Pid>) {
    gs::write_u32::<OFF_CURRENT_PID>(pid.map_or(u32::MAX, |p| p.raw()));
}

/// This CPU's `PerCpu`, reached through its own self-reference at `gs:[0]`; only [`init_ap`]'s caller needs it — everything else reads through [`gs`].
pub fn percpu_ptr() -> *mut PerCpu {
    gs::read_u64::<OFF_SELF_PTR>() as *mut PerCpu
}

/// Ring 0 timer fires the assembly stub has taken; written with a plain `inc` (IF clear there).
pub fn ring0_timer_fires() -> u32 {
    gs::read_u32::<OFF_RING0_TIMER_FIRES>()
}

pub fn last_seen_ring0_fires() -> u32 {
    gs::read_u32::<OFF_LAST_SEEN_RING0_FIRES>()
}

pub fn set_last_seen_ring0_fires(v: u32) {
    gs::write_u32::<OFF_LAST_SEEN_RING0_FIRES>(v);
}

/// The one-shot count this CPU just armed, for the timer stub's reload; `arch::apic` is the only caller.
pub fn set_last_armed_ticks(ticks: u32) {
    gs::write_u32::<OFF_LAST_ARMED_TICKS>(ticks);
}

/// The last byte of this CPU's idle guard page — the first byte an overflow reaches.
#[cfg(feature = "test-actuators")]
pub fn idle_guard_byte() -> u64 {
    idle_stack_top() - IDLE_STACK_SIZE as u64 - 1
}

/// Top of this CPU's idle stack.
pub fn idle_stack_top() -> u64 {
    gs::read_u64::<OFF_IDLE_STACK_TOP>()
}

/// No task on this CPU is inside a syscall; [`pack_task`] never produces this value.
const NO_SYSCALL: u64 = u64::MAX;

/// The identity a syscall bracket records: pid and tid together, since `Tid(0)` is every process's main thread.
fn pack_task(pid: u32, tid: u32) -> u64 {
    ((pid as u64) << 32) | tid as u64
}

/// Enter this CPU's syscall bracket. `arch::syscall` is the only caller.
pub fn enter_syscall() {
    gs::write_u64::<OFF_SYSCALL_TASK>(pack_task(
        gs::read_u32::<OFF_CURRENT_PID>(),
        gs::read_u32::<OFF_CURRENT_TID>(),
    ));
}

/// …and leave it.
pub fn leave_syscall() {
    gs::write_u64::<OFF_SYSCALL_TASK>(NO_SYSCALL);
}

/// Whether the task this CPU is running is inside a syscall right now, comparing identity rather than a flag since the word is per-CPU but the question is per-thread.
/// Errs false on a migrated/resumed syscall, so a panic there halts rather than hiding.
pub fn in_syscall() -> bool {
    let recorded = gs::read_u64::<OFF_SYSCALL_TASK>();
    recorded != NO_SYSCALL
        && recorded
            == pack_task(gs::read_u32::<OFF_CURRENT_PID>(), gs::read_u32::<OFF_CURRENT_TID>())
}

/// User RIP saved at last syscall entry; meaningful only while [`in_syscall`] holds.
pub fn syscall_rip() -> u64 {
    gs::read_u64::<OFF_SYSCALL_RIP>()
}

/// Syscall number saved at last syscall entry (for panic diagnostics).
pub fn syscall_num() -> u64 {
    gs::read_u64::<OFF_SYSCALL_NUM>()
}

/// User RSP saved at last syscall entry.
pub fn user_rsp() -> u64 {
    gs::read_u64::<OFF_USER_RSP>()
}

/// User RBP saved at last syscall entry (for panic diagnostics).
pub fn syscall_rbp() -> u64 {
    gs::read_u64::<OFF_SYSCALL_RBP>()
}

/// Swap the per-CPU fault state, returning the previous one; not atomic, but sound because only exception/panic entry points touch it, always with interrupts disabled.
pub fn swap_fault_state(new: CpuFaultState) -> CpuFaultState {
    let old = gs::read_u8::<OFF_FAULT_STATE>();
    gs::write_u8::<OFF_FAULT_STATE>(new as u8);
    match old {
        0 => CpuFaultState::Normal,
        1 => CpuFaultState::PageFault,
        2 => CpuFaultState::Fatal,
        3 => CpuFaultState::Panic,
        _ => CpuFaultState::Panic, // corrupted → treat as nested
    }
}

/// Set the per-CPU fault state.
pub fn set_fault_state(new: CpuFaultState) {
    gs::write_u8::<OFF_FAULT_STATE>(new as u8);
}
