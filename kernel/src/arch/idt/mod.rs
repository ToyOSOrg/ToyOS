pub(crate) mod exceptions;
mod device_irq;
mod dma_fault;
mod hda;
mod i8042;
#[cfg(feature = "boot-actuators")]
mod log_nest;
mod nmi;
pub(crate) mod spurious;
mod timer;
mod tlb;
pub(crate) mod unclaimed;
mod virtio_net;
mod virtio_sound;
mod xhci;

use core::arch::naked_asm;

use super::cpu;
use super::entry::{
    restore_user_state, ring3_naked_asm, save_user_state, Ring0Entry, Ring3Entry,
};
use super::cpu::{outb, io_wait};
use super::percpu;
use crate::sync::Lock;

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

/// The vector both PS/2 lines are routed to.
pub const I8042_VECTOR: u8 = Vector::I8042 as u8;

/// The vector an IOMMU writes into its own `FEDATA`.
pub const DMA_FAULT_VECTOR: u8 = Vector::DmaFault as u8;

/// The vector the HDA controller's message-signalled interrupt carries.
pub const HDA_VECTOR: u8 = Vector::Hda as u8;

/// The vector the virtio-sound device's MSI-X entry carries.
pub const VIRTIO_SOUND_VECTOR: u8 = Vector::VirtioSound as u8;

/// The vector `log-nested-emit` sends itself; installed only in a kernel built with `boot-actuators`.
#[cfg(feature = "boot-actuators")]
pub const LOG_NEST_VECTOR: u8 = 0x27;

const PF_PRESENT: u64 = 1 << 0;
const PF_WRITE: u64 = 1 << 1;
const PF_INSTRUCTION_FETCH: u64 = 1 << 4;

// The ring `cs` names is `toyos_userbound::Ring`'s to decide; nothing else re-reads the RPL bits.

#[repr(C)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtEntry {
    const EMPTY: Self = Self {
        offset_low: 0,
        selector: 0,
        ist: 0,
        type_attr: 0,
        offset_mid: 0,
        offset_high: 0,
        reserved: 0,
    };

    /// A gate for a handler that can reach another task.
    fn ring3(entry: Ring3Entry) -> Self {
        Self::at(entry.addr())
    }

    /// A gate for one that cannot.
    fn ring0(entry: Ring0Entry) -> Self {
        Self::at(entry.addr())
    }

    /// Private, so no slot can be filled by a pointer nobody classified.
    fn at(handler: u64) -> Self {
        Self {
            offset_low: handler as u16,
            selector: 0x08, // kernel CS
            ist: 0,
            type_attr: 0x8E, // interrupt gate, DPL=0, present
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }

    fn with_ist(mut self, ist_index: u8) -> Self {
        self.ist = ist_index;
        self
    }
}

#[repr(C, align(16))]
struct Idt {
    entries: [IdtEntry; 256],
}

static IDT: Lock<Idt> = Lock::new(Idt {
    entries: [IdtEntry::EMPTY; 256],
});

#[repr(C, packed)]
struct IdtPointer {
    limit: u16,
    base: u64,
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
macro_rules! exception_stub {
    ($stub:ident, $variant:ident, no_error_code) => {
        #[unsafe(naked)]
        extern "sysv64" fn $stub() {
            naked_asm!(
                "push 0",
                "push {vector}",
                "jmp {common}",
                vector = const Vector::$variant as usize,
                common = sym common_entry,
            );
        }
    };
    ($stub:ident, $variant:ident, error_code) => {
        #[unsafe(naked)]
        extern "sysv64" fn $stub() {
            naked_asm!(
                "push {vector}",
                "jmp {common}",
                vector = const Vector::$variant as usize,
                common = sym common_entry,
            );
        }
    };
}

/// Builds the vector enum, stubs, and `install_gates` from one table so the slot, stub, and dispatch arm cannot disagree.
macro_rules! idt_vectors {
    (
        dispatched { $($ex:ident = $exnum:literal, $stub:ident, $err:ident $(, ist $ist:literal)?;)* }
        direct { $($ring:tt $direct:ident = $dnum:literal, $entry:path $(, ist $dist:literal)?;)* }
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
                    _ => panic!("dispatch on vector {:#x}, which has no dispatched gate", v),
                }
            }
        }

        $(exception_stub!($stub, $ex, $err);)*

        /// Every vector whose row carries an `ist` index, paired with the index.
        const IST_VECTORS: &[(usize, u8)] = &[
            $($((Vector::$ex as usize, $ist),)?)*
            $($((Vector::$direct as usize, $dist),)?)*
        ];

        fn install_gates(idt: &mut Idt) {
            $(
                idt.entries[Vector::$ex as usize] =
                    IdtEntry::ring3(Ring3Entry::new($stub))$(.with_ist($ist))?;
            )*
            $(
                idt.entries[Vector::$direct as usize] =
                    direct_gate!($ring, $entry)$(.with_ist($dist))?;
            )*
        }
    };
}

/// The two answers a `direct` row may give.
macro_rules! direct_gate {
    (ring3, $entry:path) => {
        IdtEntry::ring3(Ring3Entry::new($entry))
    };
    (ring0, $entry:path) => {
        IdtEntry::ring0(Ring0Entry::declare($entry))
    };
}

// Every non-reserved vector needs a gate: a missing one escalates to #DF.
idt_vectors! {
    dispatched {
        DivideError        = 0x00, stub_de, no_error_code;
        Debug              = 0x01, stub_db, no_error_code;
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
        // ring0: reschedules nothing. ist 2: SYSCALL's brief CPL0-on-user-stack window would fault SMAP without it.
        ring0 Nmi          = 0x02, nmi::nmi_entry, ist 2;
        ring3 Timer        = 0x20, timer::timer_entry;
        ring3 Xhci         = 0x21, xhci::xhci_entry;
        ring3 VirtioNet    = 0x22, virtio_net::virtio_net_entry;
        ring3 VirtioSound  = 0x23, virtio_sound::virtio_sound_entry;
        ring3 I8042        = 0x24, i8042::i8042_entry;
        ring3 DmaFault     = 0x25, dma_fault::dma_fault_entry;
        ring3 Hda          = 0x26, hda::hda_entry;
        // Ring 0 because it never returns: `cli; hlt` forever.
        ring0 HaltAll      = 0xFD, stub_halt_all;
        ring3 TlbFlush     = 0xFE, tlb::tlb_flush_entry;
        // Named via the SVR, not Intel's vector list; ring0 because the handler reaches no task.
        ring0 Spurious     = 0xFF, spurious::spurious_entry;
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

/// Halt IPI from `halt_all_cpus()`; never returns.
#[unsafe(naked)]
extern "sysv64" fn stub_halt_all() {
    naked_asm!("cli", "2: hlt", "jmp 2b");
}

/// Every exception vector's second half; the bracket exists because `trap_dispatch` can return through another task, which would otherwise leak that task's registers to Ring 3.
/// `rdi` (the frame pointer) is captured before the state-save macro moves `rsp`.
#[unsafe(naked)]
extern "sysv64" fn common_entry() {
    ring3_naked_asm!(
        "push r15", "push r14", "push r13", "push r12",
        "push r11", "push r10", "push r9",  "push r8",
        "push rbp", "push rdi", "push rsi", "push rdx",
        "push rcx", "push rbx", "push rax",
        "lock add dword ptr gs:[{preempt_count}], 1",
        "mov rdi, rsp",
        save_user_state!(),
        "call {dispatch}",
        "lock sub dword ptr gs:[{preempt_count}], 1",
        // Exit-to-user epilogue runs before the GPR pops: the call clobbers scratch regs that must not leak into user state.
        "mov r11, [rsp + {fp_bytes}]",
        "test dword ptr [r11 + 144], 3",
        "jz 9f",
        "cli",
        "call {exit_to_user}",
        "9:",
        restore_user_state!(),
        "pop rax",  "pop rbx",  "pop rcx",  "pop rdx",
        "pop rsi",  "pop rdi",  "pop rbp",
        "pop r8",   "pop r9",   "pop r10",  "pop r11",
        "pop r12",  "pop r13",  "pop r14",  "pop r15",
        "add rsp, 16",
        "iretq",
        dispatch = sym trap_dispatch,
        exit_to_user = sym kernel_exit_to_user_check,
        preempt_count = const percpu::OFF_PREEMPT_COUNT,
    );
}

/// Deferred-preempt epilogue; caller must have IF=0 on entry and it returns with IF=0.
pub(crate) extern "sysv64" fn kernel_exit_to_user_check() {
    flush_ring0_timer_fires_to_trace();
    loop {
        // A killed thread returns to Ring 3 exactly once more: never.
        crate::scheduler::exit_if_killed();
        // `do_preempt` owns clearing `need_resched`; this function never clears it itself.
        if !crate::preempt::need_resched() {
            return;
        }
        assert!(!crate::scheduler::in_schedule_self(),
            "exit-to-user inside a scheduler pass");
        // Not an IrqGuard: both loop exits must set IF, not restore a saved value.
        cpu::enable_interrupts();
        crate::scheduler::do_preempt();
        cpu::disable_interrupts();
        flush_ring0_timer_fires_to_trace();
    }
}

fn flush_ring0_timer_fires_to_trace() {
    let cur = percpu::ring0_timer_fires();
    let missed = cur.wrapping_sub(percpu::last_seen_ring0_fires());
    if missed > 0 {
        crate::trace::trace(crate::trace::Kind::TimerFireBurst, missed);
        percpu::set_last_seen_ring0_fires(cur);
    }
}

/// Routes by vector to the appropriate handler; #DF and #MC get dedicated arms because they are aborts with no instruction to return to.
/// #DB stays on the default arm: nothing in this kernel arms a watchpoint to resume from it.
extern "sysv64" fn trap_dispatch(frame: *mut TrapFrame) {
    #[cfg(feature = "df-witness")]
    crate::arch::cpu::df_witness("trap_dispatch");
    // SAFETY: `frame` is `rsp` from common_entry's pushes on this CPU's kernel stack, held nowhere else.
    let frame = unsafe { &mut *frame };
    // **First, and before every handler below it.** A handler that faults again
    // is a double and then a triple fault, and a triple fault is a reset that
    // wrote nothing; the registers are what this entry can say without risking
    // that, and the report overwrites them if the machine gets that far. Every
    // vector, the page fault included: a diagnostic that skips the commonest
    // fault class is one that misses the answer, and a returning fault's record
    // is simply overwritten by the next.
    crate::blackbox::record_fault(&fault_of(frame));
    match Vector::from_raw(frame.vector) {
        Vector::DoubleFault => exceptions::double_fault_handler(frame),
        Vector::MachineCheck => exceptions::machine_check_handler(frame),
        Vector::PageFault => {
            cpu::enable_interrupts();
            exceptions::page_fault_handler(frame);
            cpu::disable_interrupts();
        }
        _ => exceptions::exception_handler(frame),
    }
}

/// This CPU's state as the black box records it, read out of the frame the stub
/// pushed and the three control registers the frame does not carry.
///
/// `gs` is not trusted for the CPU id: this runs on a machine that has already
/// faulted once, and a per-CPU block that is what faulted would fault again here.
fn fault_of(frame: &TrapFrame) -> toyos_blackbox::Fault {
    toyos_blackbox::Fault {
        vector: frame.vector,
        error_code: frame.error_code,
        rip: frame.rip,
        rsp: frame.rsp,
        rflags: frame.rflags,
        cr2: cpu::read_cr2(),
        cr3: cpu::read_cr3(),
        cpu: if crate::log::PERCPU_READY.load(core::sync::atomic::Ordering::Relaxed) {
            u64::from(percpu::cpu_id())
        } else {
            toyos_blackbox::Fault::NO_CPU
        },
        registers: [
            frame.rax, frame.rbx, frame.rcx, frame.rdx, frame.rsi, frame.rdi, frame.rbp,
            frame.r8, frame.r9, frame.r10, frame.r11, frame.r12, frame.r13, frame.r14, frame.r15,
        ],
    }
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

pub fn init() {
    disable_pic();

    install_gates(&mut IDT.lock());
    #[cfg(feature = "boot-actuators")]
    install_actuator_gates(&mut IDT.lock());
    // Every slot no row filled: delivery through a P = 0 gate is a
    // contributory fault, and the machine would halt as #DF with no name.
    let mut unclaimed = 0u32;
    for entry in IDT.lock().entries.iter_mut() {
        if entry.type_attr == 0 {
            *entry = IdtEntry::ring0(Ring0Entry::declare(unclaimed::unclaimed_entry));
            unclaimed += 1;
        }
    }
    // Negative control: clears only the IST byte on vector 2's gate, keeping the handler and ring intact.
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::nmi_without_ist() {
        IDT.lock().entries[Vector::Nmi as usize].ist = 0;
    }

    let ptr = IdtPointer {
        limit: (core::mem::size_of::<Idt>() - 1) as u16,
        base: IDT.data_ptr() as u64,
    };

    // SAFETY: `ptr` is a valid IDT descriptor built from `size_of::<Idt>()` and the just-filled static IDT.
    unsafe {
        cpu::lidt(&ptr as *const IdtPointer as *const u8);
    }

    // The table this CPU is now loaded with: how many vectors carry a handler
    // this kernel named, and how many carry only the unclaimed stub, which is
    // what a machine delivering an interrupt nobody declared would land on.
    let entries = IDT.lock().entries.len() as u32;
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

/// The one gate outside the table: only an actuator raises [`LOG_NEST_VECTOR`], so a shipping kernel never installs it.
#[cfg(feature = "boot-actuators")]
fn install_actuator_gates(idt: &mut Idt) {
    idt.entries[LOG_NEST_VECTOR as usize] =
        IdtEntry::ring3(Ring3Entry::new(log_nest::log_nest_entry));
}

/// Take IF=1 on this CPU; split from `init` so `ioapic::init` can mask firmware-left entries before interrupts are live.
pub fn enable_interrupts() {
    cpu::enable_interrupts();
}
