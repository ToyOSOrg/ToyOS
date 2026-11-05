//! The machine's census: every reading of the whole machine this kernel
//! says, in one shape on every boot, taken once, where the machine ends.
//!
//! The stop logs it as records ([`log`]). No process's start or end takes one
//! (`crate::process`'s header).

use core::fmt;

/// Every line of the census, oldest reading first: the deliveries before the
/// shootdowns' issuer total that bounds them.
fn each(mut say: impl FnMut(fmt::Arguments<'_>)) {
    crate::irq_census::census(&mut say);
    crate::arch::tlb::census(&mut say);
    crate::arch::trap::unclaimed_census(&mut say);
    crate::drivers::panic_console::census(&mut say);
}

/// The stop's: one record a line.
pub fn log() {
    each(|line| crate::log!("{line}"));
}
