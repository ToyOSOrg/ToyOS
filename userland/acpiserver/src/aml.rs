//! Everything only the machine's AML can answer. Stage 1 interprets no AML,
//! so it answers what an empty namespace does: no embedded-controller query
//! is served.

/// An embedded-controller query, taken off the controller, run once the
/// drain that took it has ended: a query's method may itself talk to the
/// controller.
pub fn query(_q: u8) {}
