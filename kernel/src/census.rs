//! The machine's census: every reading of the whole machine this kernel
//! says, in one shape on every boot, taken once, where the machine ends.
//!
//! A machine ends at its stop or at its death, and both take it: the stop
//! logs it as records ([`log`]), and a panic, the hard-lockup detector and the
//! boot deadline write it into the record they seal ([`Sealed`]). No process's
//! start or end takes one (`crate::process`'s header).
//!
//! **Every reading here is a relaxed load of an atomic**: no lock, no
//! allocation, no device, nothing that can panic. A death seals from an NMI or
//! an interrupt entry, on a machine whose every lock may be held, and may not
//! log there either, since it can have interrupted the log's own commit: so
//! each source hands its lines to a sink, and the sink decides where they go.
//! A source added here keeps to that or does not go in.

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

/// A death's: the same lines as text, for the record it seals.
///
/// Within [`toyos_blackbox::CENSUS_BYTES`], because the record is one page and
/// the lines grow with `MAX_CPUS`: a line that does not fit is dropped whole
/// with every one after it, and the count is said under the ones kept
/// ([`toyos_blackbox::Whole`]).
pub struct Sealed;

impl fmt::Display for Sealed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut lines = toyos_blackbox::Whole::within(
            f,
            toyos_blackbox::CENSUS_BYTES,
            toyos_blackbox::CENSUS_DROPPED_OPENS_WITH,
        );
        each(|line| lines.put(line));
        lines.close();
        Ok(())
    }
}
