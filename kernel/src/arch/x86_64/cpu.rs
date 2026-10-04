//! Thin wrappers around x86-64 instructions Rust has no safe operation for.
//! A wrapper is `unsafe fn` when the caller can choose a value that breaks
//! the machine, and safe otherwise: discarding a TLB entry ([`invlpg`],
//! [`invpcid`]) cannot make an access unsound, and a port read ([`inb`]) has
//! no value to get wrong.

use core::arch::asm;

use super::control_regs::PcidActive;

#[inline]
pub fn rdmsr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: writes only eax/edx; #GP on an unimplemented MSR is the caller's msr choice.
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem, nostack));
    }
    (high as u64) << 32 | low as u64
}

/// # Safety
/// The caller owns which MSR this writes and what the machine holds after — clearing `IA32_EFER.NXE` silently drops W^X kernel-wide.
#[inline]
pub unsafe fn wrmsr(msr: u32, value: u64) {
    let low = value as u32;
    let high = (value >> 32) as u32;
    asm!("wrmsr", in("ecx") msr, in("eax") low, in("edx") high, options(nomem, nostack));
}

/// Every earlier load and store done before what follows, a `wrmsr` among it:
/// Linux's `weak_wrmsr_fence`. No `nomem`, so the compiler moves no access
/// across it either.
#[inline]
pub fn wrmsr_fence() {
    // SAFETY: two fences; no register, flag or memory changes.
    unsafe { asm!("mfence", "lfence", options(nostack, preserves_flags)) };
}

#[inline]
pub fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    // SAFETY: reads the time-stamp counter into the declared outputs; not privileged since CR4.TSD is clear.
    unsafe {
        asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack));
    }
    (hi as u64) << 32 | lo as u64
}

/// `CPUID.01H:ECX[30]`; without it `RDRAND` is `#UD`, so [`rdrand`] may not be
/// reached at all.
#[inline]
pub fn has_rdrand() -> bool {
    cpuid(1, 0).2 & (1 << 30) != 0
}

/// Attempts before reporting no data: the Intel SDM's own example figure
/// (Vol. 1, "Random Number Generator Instructions"), quoted and not measured.
pub const RDRAND_ATTEMPTS: u32 = 10;

/// One drawn `u64`, or `None` after [`RDRAND_ATTEMPTS`] reported no data.
/// **Never spins**: a CPU whose CF stays clear would otherwise wedge the
/// machine wherever this was called, and no caller chooses to wait.
#[inline]
pub fn rdrand() -> Option<u64> {
    for _ in 0..RDRAND_ATTEMPTS {
        let val: u64;
        let ok: u8;
        // SAFETY: writes one register and CF, which `setc` reads back into the
        // second declared output; there is no caller-chosen value.
        unsafe {
            asm!(
                "rdrand {val}",
                "setc {ok}",
                val = out(reg) val,
                ok = out(reg_byte) ok,
                options(nomem, nostack),
            );
        }
        if ok != 0 {
            return Some(val);
        }
    }
    None
}

#[inline]
pub fn stack_pointer() -> u64 {
    let rsp: u64;
    // SAFETY: register-to-register mov into the declared output; nostack holds because rsp is read, not used.
    unsafe {
        asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack));
    }
    rsp
}

/// The direction flag; must stay clear in Ring 0, and this is the instrument that catches a stray writer.
#[cfg(feature = "df-witness")]
pub fn direction_flag_set() -> bool {
    let rflags: u64;
    // SAFETY: balanced push/pop leaves rsp unchanged; the pair uses the stack, so no nomem.
    unsafe {
        asm!("pushfq", "pop {}", out(reg) rflags, options(nomem));
    }
    rflags & 0x400 != 0
}

/// Clears DF before reporting: `core::fmt`'s backward copy would corrupt the report it's printing.
#[cfg(feature = "df-witness")]
#[cold]
#[inline(never)]
pub fn df_witness(site: &str) {
    if !direction_flag_set() {
        return;
    }
    // SAFETY: the observation is already made; only core::fmt and the log follow, which must not run with DF set.
    unsafe { asm!("cld", options(nomem, nostack)) };
    crate::hw::report_contexts(stack_pointer(), None);
    panic!(
        "DF WITNESS: cpu{} reached {site} with the direction flag set. \
         `compiler_builtins::mem::memmove`'s overlapping-copy path holds it across \
         `rep movsb`/`rep movsq`/`rep movsb` with interrupts enabled, and it is the one \
         `std` a linear disassembly of this kernel's `.text` puts on an executable path; \
         every `rep movs`/`rep stos` reached from here writes backwards.",
        crate::arch::percpu::cpu_id(),
    );
}

/// CPUID.0's vendor string, as `toyos_cpuvuln` names it.
pub fn vendor() -> toyos_cpuvuln::Vendor {
    let (_, ebx, ecx, edx) = cpuid(0, 0);
    toyos_cpuvuln::Vendor::from_id(&core::array::from_fn(|i| [ebx, edx, ecx][i / 4].to_le_bytes()[i % 4]))
}

/// CPUID.6's EAX and ECX, the thermal and power leaf; zero where CPUID.0
/// does not reach it, as `init_scattered_cpuid_features` reads it: a level
/// past the range's own is no level.
pub fn leaf_6() -> (u32, u32) {
    if (6..=0xFFFF).contains(&cpuid(0, 0).0) {
        let (eax, _, ecx, _) = cpuid(6, 0);
        (eax, ecx)
    } else {
        (0, 0)
    }
}

/// CPUID.(7,0)'s four registers, the structured extended features; zero
/// where CPUID.0 does not reach leaf 7, since a leaf above the maximum answers
/// with the highest basic leaf's data instead of faulting.
pub fn leaf_7() -> (u32, u32, u32, u32) {
    if cpuid(0, 0).0 >= 7 { cpuid(7, 0) } else { (0, 0, 0, 0) }
}

/// CPUID with both index registers, as `(eax, ebx, ecx, edx)`.
///
/// `core`'s intrinsic and not an `asm!` of our own: `rbx` is CPUID's output
/// and LLVM's reserved register, yet LLVM may still allocate it to a `reg`
/// operand, so a hand-written save of `rbx` around a compiler-chosen
/// register can lose the output.
pub fn cpuid(leaf: u32, subleaf: u32) -> (u32, u32, u32, u32) {
    let r = core::arch::x86_64::__cpuid_count(leaf, subleaf);
    (r.eax, r.ebx, r.ecx, r.edx)
}

#[inline]
pub fn read_cr0() -> u64 {
    let value: u64;
    // SAFETY: reads a control register in Ring 0; touches no memory and cannot fault.
    unsafe { asm!("mov {}, cr0", out(reg) value, options(nomem, nostack)); }
    value
}

#[inline]
pub fn read_cr4() -> u64 {
    let value: u64;
    // SAFETY: read_cr0's argument.
    unsafe { asm!("mov {}, cr4", out(reg) value, options(nomem, nostack)); }
    value
}

/// # Safety
/// Writes all of CR0, so the caller owns the whole machine configuration, not one flag of it.
#[inline]
pub unsafe fn write_cr0(value: u64) {
    asm!("mov cr0, {}", in(reg) value, options(nostack));
}

/// # Safety
/// `#GP` on a bit this CPU doesn't define, on clearing PAE or LA57 in long mode, or on taking PCIDE 0→1 while CR3[11:0] is non-zero (SDM Vol. 3A §4.10.1).
#[inline]
pub unsafe fn write_cr4(value: u64) {
    asm!("mov cr4, {}", in(reg) value, options(nostack));
}

/// # Safety
/// Correct only inside a no-fill cache window: CR0.CD set and CR0.NW clear (SDM Vol. 3A §11.5.3).
#[inline]
pub unsafe fn wbinvd() {
    asm!("wbinvd", options(nostack, preserves_flags));
}

/// Clears `RFLAGS.AC`, so a supervisor access to a user page faults under SMAP.
#[inline]
pub fn clac() {
    // SAFETY: clears one RFLAGS bit and touches no memory; #UD without SMAP, which the one caller checks first.
    unsafe { asm!("clac", options(nomem, nostack)); }
}

#[inline]
pub fn read_cr2() -> u64 {
    let value: u64;
    // SAFETY: read_cr0's argument; the value is meaningful only on a #PF.
    unsafe { asm!("mov {}, cr2", out(reg) value, options(nomem, nostack)); }
    value
}

#[inline]
pub fn read_cr3() -> u64 {
    let value: u64;
    // SAFETY: read_cr0's argument.
    unsafe { asm!("mov {}, cr3", out(reg) value, options(nomem, nostack)); }
    value
}

/// # Safety
/// The caller must ensure the value is a valid CR3.
#[inline]
pub unsafe fn write_cr3(value: u64) {
    asm!("mov cr3, {}", in(reg) value, options(nostack));
}

/// Discard this CPU's translation for one linear address; safe because discarding can't make an access unsound.
#[inline]
pub fn invlpg(addr: u64) {
    // SAFETY: one instruction with no memory operand the compiler can see.
    unsafe { asm!("invlpg [{}]", in(reg) addr, options(nostack)); }
}

/// Descriptor type for [`invpcid`] (SDM Vol. 3A §4.10.4.1); three variants, not four, because this kernel issues no global-page discard.
#[derive(Clone, Copy)]
#[repr(u64)]
pub enum Invpcid {
    /// One linear address, in one PCID.
    Address = 0,
    /// Every entry tagged with one PCID, global pages excepted.
    SinglePcid = 1,
    /// Every entry in every PCID, global pages included.
    AllIncludingGlobal = 2,
}

/// Discard TLB entries by descriptor type; both instruction faults are excluded by the signature ([`Invpcid`], [`PcidActive`]).
#[inline]
pub fn invpcid(_have: PcidActive, kind: Invpcid, pcid: u16, addr: u64) {
    let desc: [u64; 2] = [pcid as u64, addr];
    // SAFETY: desc is a readonly local sixteen bytes the CPU reads and nothing writes.
    unsafe {
        asm!(
            "invpcid {0}, [{1}]",
            in(reg) kind as u64,
            in(reg) desc.as_ptr(),
            options(nostack, readonly),
        );
    }
}

/// # Safety
/// The pointer must reference a valid IDT descriptor.
#[inline]
pub unsafe fn lidt(ptr: *const u8) {
    asm!("lidt [{}]", in(reg) ptr, options(nostack));
}

/// # Safety
/// The selector must reference a valid TSS entry in the GDT.
#[inline]
pub unsafe fn ltr(selector: u16) {
    asm!("ltr {:x}", in(reg) selector as u64, options(nostack));
}

/// `sti`; not a drop-in for a caller whose `cli`/`sti` also needs the compiler barrier a bare `asm!` carries.
#[inline]
pub fn enable_interrupts() {
    #[cfg(feature = "mask-windows")]
    if !interrupts_enabled() {
        crate::windows::irqs_unmasking();
    }
    // SAFETY: one RFLAGS bit, no memory, matching the options declared above.
    unsafe {
        asm!("sti", options(nomem, nostack));
    }
}

/// Whether this CPU would take an interrupt here — `RFLAGS.IF`.
///
/// **A measurement and not a guard.** The answer is stale the instant any `sti`
/// or `cli` runs, so nothing may branch on it to decide whether to mask; what it
/// is for is a site that has to *say* which state it inherited, which is a fact
/// about its caller rather than about the instruction after it.
#[cfg(any(feature = "boot-actuators", feature = "mask-windows"))]
pub fn interrupts_enabled() -> bool {
    let rflags: u64;
    // SAFETY: balanced push/pop leaves rsp unchanged; the pair uses the stack, so no nomem.
    unsafe {
        asm!("pushfq", "pop {}", out(reg) rflags, options(nomem));
    }
    rflags & (1 << 9) != 0
}

/// `cli`, with [`enable_interrupts`]'s caveat about the missing barrier.
#[inline]
pub fn disable_interrupts() {
    #[cfg(feature = "mask-windows")]
    let were_open = interrupts_enabled();
    // SAFETY: enable_interrupts's argument; masking interrupts is a latency bug, not a soundness one.
    unsafe {
        asm!("cli", options(nomem, nostack));
    }
    #[cfg(feature = "mask-windows")]
    if were_open {
        crate::windows::irqs_masked();
    }
}

/// The kernel's own FS base, not `rd/wrfsbase` (`control_regs::CR4_FORBIDDEN`).
const IA32_FS_BASE: u32 = 0xC000_0100;

#[inline]
pub fn read_fs_base() -> u64 {
    rdmsr(IA32_FS_BASE)
}

/// # Safety
/// `val` becomes this CPU's FS base; #GP if non-canonical, and the caller owns which thread's thread-locals it addresses.
#[inline]
pub unsafe fn write_fs_base(val: u64) {
    // SAFETY: `IA32_FS_BASE` takes any canonical value; the caller owns both.
    unsafe { wrmsr(IA32_FS_BASE, val) };
}

pub fn halt() -> ! {
    loop {
        // SAFETY: cli must precede hlt with nothing between, or an interrupt in the gap wakes a CPU that never returns.
        unsafe {
            asm!("cli; hlt", options(nomem, nostack));
        }
    }
}

/// # Safety
/// No fault in Ring 0; the caller owns which device answers at `port` and what the byte commands it to do.
#[inline]
pub unsafe fn outb(port: u16, value: u8) {
    asm!("out dx, al", in("dx") port, in("al") value);
}

/// One byte from an I/O port; safe because a read has no value a caller can get wrong.
#[inline]
pub fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: one instruction into the declared output, no memory operand, no fault in Ring 0 (outb's # Safety carries why).
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port);
    }
    value
}

/// # Safety
/// `outb`'s contract, sixteen bits wide.
#[inline]
pub unsafe fn outw(port: u16, value: u16) {
    asm!("out dx, ax", in("dx") port, in("ax") value);
}

#[inline]
pub fn inw(port: u16) -> u16 {
    let value: u16;
    // SAFETY: as `inb` — one instruction into the declared output, no memory operand.
    unsafe {
        asm!("in ax, dx", out("ax") value, in("dx") port);
    }
    value
}

/// One I/O bus cycle of delay, for a device that needs one between two commands.
#[inline]
pub fn io_wait() {
    // SAFETY: port 0x80 is the unused POST diagnostic port, so this commands nothing.
    unsafe { outb(0x80, 0) };
}

/// The CPU's free-running counter: the TSC, which counts from reset.
#[inline]
pub fn counter() -> u64 {
    rdtsc()
}

/// This function's caller's frame pointer: `rbp`, which `-Cforce-frame-pointers=yes` makes one.
#[inline(always)]
pub fn frame_pointer() -> u64 {
    let rbp: u64;
    // SAFETY: register-to-register mov only.
    unsafe { asm!("mov {}, rbp", out(reg) rbp, options(nomem, nostack, preserves_flags)) };
    rbp
}

/// The TSC's frequency in hertz as CPUID *states* it, for the one caller that
/// may run before the clock is calibrated — the panic path, which has to bound a wait on a
/// machine that never reached the HPET. Nothing calibrates against it and no
/// third source is guessed at: a CPU that states neither leaf answers `None`
/// and its caller says so rather than inventing a rate.
pub fn stated_counter_hz() -> Option<u64> {
    let max_leaf = cpuid(0, 0).0;
    toyos_tsc::stated_hz(
        (max_leaf >= 0x15).then(|| cpuid(0x15, 0)),
        (max_leaf >= 0x16).then(|| cpuid(0x16, 0)),
    )
}


/// The thread pointer this CPU is running with: the FS base, which user TLS is addressed from.
#[inline]
pub fn thread_pointer() -> u64 {
    read_fs_base()
}

/// Leave the current stack for good and run `func` on the one ending at `top`,
/// with a zeroed frame chain so a panic there backtraces instead of walking off
/// the top.
/// # Safety
/// Nothing on the current stack is live past this call, and `top` is the end of
/// a stack this CPU owns.
pub unsafe fn run_on_stack(top: u64, func: extern "C" fn() -> !) -> ! {
    // SAFETY: the caller's contract; `push` leaves `rsp` where a function entry
    // expects it.
    unsafe {
        asm!(
            "mov rsp, {sp}",
            "xor ebp, ebp",
            "push rbp",
            "jmp {func}",
            sp = in(reg) top,
            func = in(reg) func as *const () as usize,
            options(noreturn),
        );
    }
}

/// `df-witness-mutate`'s staging: set `DF` one instruction before the reader
/// that must refuse it. Inlined, so `DF` crosses no `ret` to get there.
#[cfg(feature = "df-witness-mutate")]
#[inline(always)]
pub fn df_witness_mutate() {
    // SAFETY: a build that exists to stage the defect, and the reader after it
    // panics before any `rep movs` can run. Nothing runs in between, so no
    // string op ever executes with it set.
    unsafe { asm!("std", options(nomem, nostack)) };
}

/// This CPU's APIC id, from CPUID: the id the hardware gives it, readable
/// before any per-CPU state exists.
///
/// Not `rdmsr(IA32_X2APIC_APICID)`: that MSR is `#GP` before `irqchip::init_ap`
/// has run, and a panic an AP takes before then must not fault inside the
/// reentry guard.
pub fn hardware_id() -> u32 {
    let (max_leaf, _, _, _) = cpuid(0, 0);
    for leaf in [0x1F, 0x0B] {
        if max_leaf >= leaf {
            let (_, ebx, _, edx) = cpuid(leaf, 0);
            // SDM Vol. 2A, CPUID leaf 0BH: EBX[15:0] == 0 means unimplemented,
            // not id 0, so the leaf-1 fallback below must still run.
            if ebx & 0xFFFF != 0 {
                return edx;
            }
        }
    }
    let (_, ebx, _, _) = cpuid(1, 0);
    ebx >> 24
}
