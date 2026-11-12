//! `nmi-user-gs`: vector 2 entered at CPL 0 with a user's GS base live, the
//! state between an entry and its `swapgs` and after an exit's, staged from
//! ordinary context. It reports, so it is not in `nmi.rs`, whose whole
//! contents run inside an NMI and may not log.

use crate::arch::{cpu, percpu};
use crate::irq_census::{self, Source};

/// Stages the entry once under a zeroed kernel-heap block as the user's GS
/// base, a kernel-half address as `WRGSBASE` may give one. The handler has to
/// count itself in this CPU's block, leave the decoy untouched, and return to
/// the base it found.
pub fn selftest() {
    let cpu = percpu::cpu_id();
    let Some(before) = irq_census::deliveries(cpu, Source::Nmi) else {
        crate::log!("nmi-user-gs: FAIL — cpu{cpu} publishes no census block");
        return;
    };
    let decoy = alloc::vec![0u64; core::mem::size_of::<percpu::PerCpu>().div_ceil(8)];
    let staged = decoy.as_ptr() as u64;
    let returned_to;
    {
        // Closed across the swapped span: a maskable gate there would read a
        // Ring 0 frame and keep the decoy.
        let _masked = crate::arch::IrqGuard::close();
        let kept = cpu::read_user_gs_base();
        // SAFETY: the swapped span is `int 2` alone, which touches no
        // GS-relative memory of this code's; the second `swapgs` leaves in
        // `IA32_KERNEL_GS_BASE` the base the handler returned to, and this
        // thread's user GS base goes back there before interrupts open.
        unsafe {
            cpu::write_user_gs_base(staged);
            core::arch::asm!("swapgs", "int 2", "swapgs");
            returned_to = cpu::read_user_gs_base();
            cpu::write_user_gs_base(kept);
        }
    }
    let after = irq_census::deliveries(cpu, Source::Nmi).unwrap_or(before);
    let touched = decoy.iter().filter(|word| **word != 0).count();
    let verdict = if after > before && touched == 0 && returned_to == staged { "PASS" } else { "FAIL" };
    crate::log!(
        "nmi-user-gs: {verdict} — vector 2 at CPL 0 under GS base {staged:#x}: cpu{cpu}'s own count \
         {before} -> {after}, {touched} decoy words written, returned to GS base {returned_to:#x}"
    );
}
