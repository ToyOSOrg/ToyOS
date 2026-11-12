pub(crate) mod exceptions;
mod debug;
mod device_irq;
mod dma_fault;
mod isa;
mod kick;
mod nmi;
#[cfg(feature = "boot-actuators")]
mod nmi_user_gs;
pub(crate) mod spurious;
mod timer;
mod tlb;
pub(crate) mod unclaimed;
pub(crate) mod user_dev;

pub(crate) use user_dev::ClaimVector;

use core::arch::naked_asm;

use super::cpu;
use super::entry::{gate_iretq, gate_prefix, install_kernel_gs, ring3_naked_asm, GATE_PREFIX};
use super::cpu::{outb, io_wait};
use super::pio::{Port, PIC_PRIMARY, PIC_SECONDARY};
use super::percpu;
use crate::sync::Lock;

const PIC1_CMD: Port = PIC_PRIMARY.port(0);
const PIC1_DATA: Port = PIC_PRIMARY.port(1);
const PIC2_CMD: Port = PIC_SECONDARY.port(0);
const PIC2_DATA: Port = PIC_SECONDARY.port(1);

/// The vector `apic::kick_cpu` raises.
pub const KICK_VECTOR: u8 = Vector::Kick as u8;

/// The vector each `isa` row's lines are routed to, by row: the vector is
/// how the kernel knows whose record an interrupt belongs to.
pub const ISA_VECTORS: [u8; crate::isa::MAX_ROWS] = [Vector::Isa0 as u8, Vector::Isa1 as u8, Vector::Isa2 as u8];

/// The vector an IOMMU writes into its own `FEDATA`.
pub const DMA_FAULT_VECTOR: u8 = Vector::DmaFault as u8;

const PF_PRESENT: u64 = 1 << 0;
const PF_WRITE: u64 = 1 << 1;
const PF_INSTRUCTION_FETCH: u64 = 1 << 4;

// The ring `cs` names is `toyos_userbound::Ring`'s to decide; nothing else re-reads the RPL bits.

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtEntry {
    pub(crate) const EMPTY: Self = Self {
        offset_low: 0,
        selector: 0,
        ist: 0,
        type_attr: 0,
        offset_mid: 0,
        offset_high: 0,
        reserved: 0,
    };

    /// An interrupt gate onto `entry`.
    fn at(entry: unsafe extern "sysv64" fn()) -> Self {
        Self::at_address(entry as *const () as u64)
    }

    /// The address this gate enters at.
    fn target(&self) -> u64 {
        u64::from(self.offset_low) | u64::from(self.offset_mid) << 16 | u64::from(self.offset_high) << 32
    }

    /// An interrupt gate onto the code at `handler`.
    fn at_address(handler: u64) -> Self {
        Self::gate(handler, 0x08) // kernel CS
    }

    /// An interrupt gate onto `handler` through code segment `selector`.
    pub(crate) fn gate(handler: u64, selector: u16) -> Self {
        Self {
            offset_low: handler as u16,
            selector,
            ist: 0,
            type_attr: 0x8E, // interrupt gate, DPL=0, present
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }

    /// DPL 3: what `INT n` from Ring 3 may name (SDM Vol. 3A §6.12.1.1);
    /// any other gate it names raises `#GP` instead.
    fn reachable_from_ring3(mut self) -> Self {
        self.type_attr = 0xEE; // interrupt gate, DPL=3, present
        self
    }

    fn with_ist(mut self, ist_index: u8) -> Self {
        self.ist = ist_index;
        self
    }

    /// The same gate entered past its prefix (`entry::gate_prefix`), which
    /// it is refused unless it opens with: the bytes of the `test` of the
    /// frame's `CS`, the short `jz` over the call and the call.
    fn past_prefix(self) -> Self {
        let gate = self.target();
        // SAFETY: a gate's first bytes are kernel text, mapped readable.
        let opens = unsafe { core::ptr::read(gate as *const [u8; GATE_PREFIX]) };
        assert!(
            matches!(opens, [0xF6, 0x44, 0x24, _, 0x03, 0x74, 0x05, 0xE8, ..]),
            "idt: the gate at {gate:#x} opens with {opens:02x?}, not the branch history prefix"
        );
        let past = gate + GATE_PREFIX as u64;
        Self { offset_low: past as u16, offset_mid: (past >> 16) as u16, offset_high: (past >> 32) as u32, ..self }
    }
}

#[repr(C, align(16))]
struct Idt {
    entries: [IdtEntry; 256],
}

/// Every gate entered at its prefix: the table a CPU whose decision names the
/// branch history clear loads.
static AT_PREFIX: Lock<Idt> = Lock::new(Idt {
    entries: [IdtEntry::EMPTY; 256],
});

/// Every gate entered past its prefix: every other CPU's table.
static PAST_PREFIX: Lock<Idt> = Lock::new(Idt {
    entries: [IdtEntry::EMPTY; 256],
});

#[repr(C, packed)]
pub(crate) struct IdtPointer {
    pub(crate) limit: u16,
    pub(crate) base: u64,
}

/// Field order mirrors `common_entry`'s push order (reversed) and the CPU's own frame; both must change together.
#[repr(C)]
pub struct TrapFrame {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub vector: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

/// Pushes the vector, and a zero error code where the CPU pushes none, so [`TrapFrame`] has one shape.
/// Misclassifying a vector's error-code column shifts every `TrapFrame` field above `error_code` by eight bytes.
/// A vector on an IST stack goes on to [`fatal_entry`], every other to [`common_entry`].
macro_rules! exception_stub {
    ($stub:ident, $variant:ident, $err:ident) => {
        exception_stub!(@ $stub, $variant, $err, common_entry);
    };
    ($stub:ident, $variant:ident, $err:ident, ist $ist:literal) => {
        exception_stub!(@ $stub, $variant, $err, fatal_entry);
    };
    (@ $stub:ident, $variant:ident, no_error_code, $common:ident) => {
        #[unsafe(naked)]
        extern "sysv64" fn $stub() {
            naked_asm!(
                gate_prefix!(),
                "push 0",
                "push {vector}",
                "jmp {common}",
                vector = const Vector::$variant as usize,
                common = sym $common,
                gate_cs = const 8,
                clear_branch_history = sym super::mitigation::clear_branch_history,
            );
        }
    };
    (@ $stub:ident, $variant:ident, error_code, $common:ident) => {
        #[unsafe(naked)]
        extern "sysv64" fn $stub() {
            naked_asm!(
                gate_prefix!(),
                "push {vector}",
                "jmp {common}",
                vector = const Vector::$variant as usize,
                common = sym $common,
                gate_cs = const 16,
                clear_branch_history = sym super::mitigation::clear_branch_history,
            );
        }
    };
}

/// Builds the vector enum, stubs, and `install_gates` from one table so the slot, stub, and dispatch arm cannot disagree.
macro_rules! idt_vectors {
    (
        dispatched { $($ex:ident = $exnum:literal, $stub:ident, $err:ident $(, ist $ist:literal)?;)* }
        direct { $($direct:ident = $dnum:literal, $entry:path $(, ist $dist:literal)?;)* }
    ) => {
        /// IDT vector assignments — CPU exceptions and hardware interrupts.
        #[repr(usize)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Vector {
            $($ex = $exnum,)*
            $($direct = $dnum,)*
        }

        impl Vector {
            /// The number a stub pushed; the wildcard arm is unreachable for any vector this module installs.
            fn from_raw(v: u64) -> Self {
                match v {
                    $($exnum => Self::$ex,)*
                    $($dnum => Self::$direct,)*
                    _ => panic!("dispatch on vector {:#x}, which has no dispatched gate", v),
                }
            }
        }

        $(exception_stub!($stub, $ex, $err $(, ist $ist)?);)*

        /// Every vector whose row carries an `ist` index, paired with the index.
        const IST_VECTORS: &[(usize, u8)] = &[
            $($((Vector::$ex as usize, $ist),)?)*
            $($((Vector::$direct as usize, $dist),)?)*
        ];

        fn install_gates(idt: &mut Idt) {
            $(
                idt.entries[Vector::$ex as usize] = IdtEntry::at($stub)$(.with_ist($ist))?;
            )*
            $(
                idt.entries[Vector::$direct as usize] = IdtEntry::at($entry)$(.with_ist($dist))?;
            )*
        }
    };
}

// Every non-reserved vector needs a gate: a missing one escalates to #DF.
idt_vectors! {
    dispatched {
        DivideError        = 0x00, stub_de, no_error_code;
        Breakpoint         = 0x03, stub_bp, no_error_code;
        Overflow           = 0x04, stub_of, no_error_code;
        BoundRange         = 0x05, stub_br, no_error_code;
        InvalidOpcode      = 0x06, stub_ud, no_error_code;
        DeviceNotAvailable = 0x07, stub_nm, no_error_code;
        DoubleFault        = 0x08, stub_df, error_code, ist 1;
        InvalidTss         = 0x0A, stub_ts, error_code;
        SegmentNotPresent  = 0x0B, stub_np, error_code;
        StackSegment       = 0x0C, stub_ss, error_code;
        GeneralProtection  = 0x0D, stub_gp, error_code;
        PageFault          = 0x0E, stub_pf, error_code;
        X87FloatingPoint   = 0x10, stub_mf, no_error_code;
        AlignmentCheck     = 0x11, stub_ac, error_code;
        MachineCheck       = 0x12, stub_mc, no_error_code, ist 3;
        SimdFloatingPoint  = 0x13, stub_xm, no_error_code;
        Virtualization     = 0x14, stub_ve, no_error_code;
        ControlProtection  = 0x15, stub_cp, error_code;
    }
    direct {
        // From Ring 3 it goes on through `common_entry` as vector 1 (`debug`).
        Debug        = 0x01, debug::debug_entry, ist 4;
        // ist 2: SYSCALL's brief CPL0-on-user-stack window would fault SMAP without it.
        Nmi          = 0x02, nmi::nmi_entry, ist 2;
        Timer        = 0x20, timer::timer_entry;
        DmaFault     = 0x25, dma_fault::dma_fault_entry;
        Isa0         = 0x2C, isa::isa0_entry;
        Isa1         = 0x2D, isa::isa1_entry;
        Isa2         = 0x2E, isa::isa2_entry;
        Kick         = 0xFC, kick::kick_entry;
        // Never returns: `cli; hlt` forever.
        HaltAll      = 0xFD, stub_halt_all;
        TlbFlush     = 0xFE, tlb::tlb_flush_entry;
        // Named via the SVR, not Intel's vector list.
        Spurious     = 0xFF, spurious::spurious_entry;
    }
}

/// Checked at compile time, not tested: a missing IST index would be invisible until the machine is already dying.
const _: () = {
    const fn ist_of(vector: usize) -> u8 {
        let mut i = 0;
        while i < IST_VECTORS.len() {
            if IST_VECTORS[i].0 == vector {
                return IST_VECTORS[i].1;
            }
            i += 1;
        }
        0
    }
    assert!(ist_of(0x08) == 1, "#DF must take its frame on IST1");
    assert!(ist_of(0x02) == 2, "an NMI must take its frame on IST2");
    assert!(ist_of(0x12) == 3, "#MC must take its frame on IST3");
    assert!(ist_of(0x01) == 4, "#DB must take its frame on IST4");
    // No two vectors may share an IST index: it is not re-entrant.
    let mut i = 0;
    while i < IST_VECTORS.len() {
        let (_, ist) = IST_VECTORS[i];
        assert!(ist >= 1 && ist as usize <= percpu::IST_STACKS, "no stack for that IST index");
        let mut j = i + 1;
        while j < IST_VECTORS.len() {
            assert!(IST_VECTORS[j].1 != ist, "two vectors share one IST stack");
            j += 1;
        }
        i += 1;
    }
};

/// Whether `rip` is where some gate of either table enters.
fn is_gate_target(rip: u64) -> bool {
    // SAFETY: the tables are written only by `init`, before any gate is taken.
    let tables = unsafe { [&*AT_PREFIX.data_ptr(), &*PAST_PREFIX.data_ptr()] };
    tables.iter().flat_map(|idt| &idt.entries).any(|gate| gate.type_attr != 0 && gate.target() == rip)
}

/// Whether `rip` is where either table enters the gate at `gate`.
fn is_gate_of(rip: u64, gate: u64) -> bool {
    rip == gate || rip == gate + GATE_PREFIX as u64
}

/// Halt IPI from `halt_all_cpus()`; never returns.
#[unsafe(naked)]
extern "sysv64" fn stub_halt_all() {
    naked_asm!(
        gate_prefix!(),
        "cli",
        "2: hlt",
        "jmp 2b",
        gate_cs = const 8,
        clear_branch_history = sym super::mitigation::clear_branch_history,
    );
}

/// Every exception vector's second half: the [`TrapFrame`], 22 words below a
/// 16-byte-aligned stack, so `rsp` is aligned for the calls.
#[unsafe(naked)]
extern "sysv64" fn common_entry() {
    // The stub's vector and error code, and `rip`, are below `cs`.
    ring3_naked_asm!(cs = 24;
        "push r15", "push r14", "push r13", "push r12",
        "push r11", "push r10", "push r9",  "push r8",
        "push rbp", "push rdi", "push rsi", "push rdx",
        "push rcx", "push rbx", "push rax",
        "add dword ptr gs:[{preempt_count}], 1",
        "mov rdi, rsp",
        "call {dispatch}",
        "sub dword ptr gs:[{preempt_count}], 1",
        // Exit-to-user epilogue runs before the GPR pops: the call clobbers scratch regs that must not leak into user state.
        "test dword ptr [rsp + 144], 3",
        "jz 9f",
        "cli",
        "call {exit_to_user}",
        "9:",
        "pop rax",  "pop rbx",  "pop rcx",  "pop rdx",
        "pop rsi",  "pop rdi",  "pop rbp",
        "pop r8",   "pop r9",   "pop r10",  "pop r11",
        "pop r12",  "pop r13",  "pop r14",  "pop r15",
        "add rsp, 16",
        gate_iretq!(),
        dispatch = sym trap_dispatch,
        exit_to_user = sym kernel_exit_to_user_check,
        preempt_count = const percpu::OFF_PREEMPT_COUNT,
    );
}

/// The second half of an exception on an IST stack, `#DF` and `#MC`: the
/// [`TrapFrame`] as [`common_entry`] lays it, with the kernel's GS base
/// installed whatever it found, since these arrive in the kernel's CPL-0
/// stretches with a user's base live (`arch::entry`). Never returns: both
/// handlers end the machine.
#[unsafe(naked)]
extern "sysv64" fn fatal_entry() {
    naked_asm!(
        "cld",
        "clac",
        "push r15", "push r14", "push r13", "push r12",
        "push r11", "push r10", "push r9",  "push r8",
        "push rbp", "push rdi", "push rsi", "push rdx",
        "push rcx", "push rbx", "push rax",
        install_kernel_gs!(),
        "mov rdi, rsp",
        "call {dispatch}",
        "ud2",
        dispatch = sym trap_dispatch,
        gs_base_msr = const percpu::MSR_GS_BASE,
        gdt_in_percpu = const percpu::OFF_GDT,
    );
}

/// Deferred-preempt epilogue; caller must have IF=0 on entry and it returns with IF=0.
pub(crate) extern "sysv64" fn kernel_exit_to_user_check() {
    crate::scheduler::exit_to_user();
}

/// Routes by vector to the appropriate handler; #DF and #MC get dedicated arms because they are aborts with no instruction to return to.
/// #DB stays on the default arm: nothing in this kernel arms a watchpoint to resume from it.
extern "sysv64" fn trap_dispatch(frame: *mut TrapFrame) {
    #[cfg(feature = "df-witness")]
    crate::arch::cpu::df_witness("trap_dispatch");
    #[cfg(feature = "test-actuators")]
    crate::arch::cpu::ac_witness("trap_dispatch");
    // SAFETY: `frame` is `rsp` from common_entry's pushes on this CPU's kernel stack, held nowhere else.
    let frame = unsafe { &mut *frame };
    match Vector::from_raw(frame.vector) {
        Vector::DoubleFault => exceptions::double_fault_handler(frame),
        Vector::MachineCheck => exceptions::machine_check_handler(frame),
        Vector::PageFault => {
            #[cfg(feature = "mask-windows")]
            windows_entered(frame);
            cpu::enable_interrupts();
            exceptions::page_fault_handler(frame);
            cpu::disable_interrupts();
            // Only a Ring 3 fault or a user copy's comes back: `common_entry`
            // lowers the count next, and `exit_to_user` or the `iretq` opens
            // interrupts.
            #[cfg(feature = "mask-windows")]
            crate::windows::preempt_lowering();
        }
        _ => {
            #[cfg(feature = "mask-windows")]
            windows_entered(frame);
            exceptions::exception_handler(frame)
        }
    }
}

/// A Ring 3 frame had interrupts open, the gate masked them, and
/// `common_entry` raised the preempt count by one. A Ring 0 frame ends the
/// machine: its CPU's record stops first, so no hook from here on refuses in
/// place of the fault's report.
#[cfg(feature = "mask-windows")]
fn windows_entered(frame: &TrapFrame) {
    if !toyos_userbound::Ring::of_cs(frame.cs).is_user() && super::uaccess::fixup(frame.rip).is_none() {
        crate::windows::stop_here();
    }
    crate::windows::irqs_masked();
    crate::windows::preempt_raised();
}

/// Disable the legacy 8259 PIC.
fn disable_pic() {
    // SAFETY: every port is one of the 8259 pair's four; kept as one block so the PIC is never left half-initialised delivering stray vectors.
    unsafe {
        outb(PIC1_CMD, 0x11);
        io_wait();
        outb(PIC2_CMD, 0x11);
        io_wait();

        outb(PIC1_DATA, 32);
        io_wait();
        outb(PIC2_DATA, 40);
        io_wait();

        outb(PIC1_DATA, 4);
        io_wait();
        outb(PIC2_DATA, 2);
        io_wait();

        outb(PIC1_DATA, 0x01);
        io_wait();
        outb(PIC2_DATA, 0x01);
        io_wait();

        outb(PIC1_DATA, 0xFF);
        outb(PIC2_DATA, 0xFF);
    }
}

/// Builds both tables and loads the one `clears_branch_history` names: the
/// boot CPU's.
pub fn init(clears_branch_history: bool) {
    disable_pic();

    install_gates(&mut AT_PREFIX.lock());
    // `INT3` from Ring 3 arrives as the `#BP` it is, and ends its process as a
    // trap, as `BRK` does on AArch64.
    {
        let mut idt = AT_PREFIX.lock();
        let breakpoint = &mut idt.entries[Vector::Breakpoint as usize];
        *breakpoint = breakpoint.reachable_from_ring3();
    }
    user_dev::install(&mut AT_PREFIX.lock());
    // Every slot no row filled: delivery through a P = 0 gate is a
    // contributory fault, and the machine would halt as #DF with no name.
    let mut unclaimed = 0u32;
    for entry in AT_PREFIX.lock().entries.iter_mut() {
        if entry.type_attr == 0 {
            *entry = IdtEntry::at(unclaimed::unclaimed_entry);
            unclaimed += 1;
        }
    }
    let gates = AT_PREFIX.lock().entries;
    PAST_PREFIX.lock().entries = gates.map(IdtEntry::past_prefix);
    let ptr = load(clears_branch_history);

    // The table this CPU is now loaded with: how many vectors carry a handler
    // this kernel named, and how many carry only the unclaimed stub, which is
    // what a machine delivering an interrupt nobody declared would land on.
    let entries = AT_PREFIX.lock().entries.len() as u32;
    // Copied out first: `IdtPointer` is packed, so a formatting argument would
    // be a reference to an unaligned field.
    let (base, limit) = (ptr.base, ptr.limit);
    crate::log!(
        "idt: {} vectors, {} declared, {} unclaimed, table at {:#x} limit {:#x}",
        entries,
        entries - unclaimed,
        unclaimed,
        base,
        limit,
    );
}

/// Loads on this CPU the table whose gates open with the branch history
/// clear where `clears_branch_history`, and the one past it elsewhere: what
/// this CPU's decision names. After [`init`].
pub fn load(clears_branch_history: bool) -> IdtPointer {
    let table = if clears_branch_history { &AT_PREFIX } else { &PAST_PREFIX };
    let ptr = IdtPointer { limit: (core::mem::size_of::<Idt>() - 1) as u16, base: table.data_ptr() as u64 };
    // SAFETY: `ptr` names one of the two static tables `init` filled whole.
    unsafe { cpu::lidt(&ptr as *const IdtPointer as *const u8) };
    ptr
}

/// Take IF=1 on this CPU; split from `init` so `ioapic::init` can mask firmware-left entries before interrupts are live.
pub fn enable_interrupts() {
    cpu::enable_interrupts();
}

/// The crash report for a panic, from the frame pointer the panic handler stood on.
pub(crate) fn report_panic(message: &core::panic::PanicInfo, frame: u64) {
    exceptions::crash_report(&exceptions::CrashInfo::Panic { message, rbp: frame });
}

/// Whether the interrupted context a trap frame's flags word describes could
/// have taken an interrupt: `RFLAGS.IF`.
pub(crate) const fn frame_interrupts_enabled(rflags: u64) -> bool {
    rflags & (1 << 9) != 0
}

/// A real `#DF`, not simulated: pushing to a non-canonical `rsp` raises `#SS`,
/// and delivering that needs another push to the same `rsp` — the `#DF`
/// condition. Non-canonical rather than unmapped: on a bigger machine an
/// unmapped address can fall inside the direct map and simply get written to.
/// Only `#DF` has an IST, so every fault on the way there lands on this same
/// unusable stack. `SYS_DEBUG`'s alone.
#[cfg(feature = "test-actuators")]
pub(crate) fn provoke_double_fault() -> ! {
    // SAFETY: unsound by design, like `SYS_DEBUG`'s null read; the #DF this
    // raises never returns here.
    unsafe {
        core::arch::asm!(
            "mov rsp, {bad}",
            "push 0",
            bad = in(reg) 0x0000_8000_0000_0000u64,
            options(noreturn),
        );
    }
}

pub(crate) use unclaimed::census as unclaimed_census;
#[cfg(feature = "boot-actuators")]
pub(crate) use nmi_user_gs::selftest as nmi_user_gs_selftest;
/// How much of the double-fault stack the crash report used, once the report is out.
pub(crate) use super::percpu::ist1_report as report_fault_stack;
