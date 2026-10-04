//! Everything only the machine's AML can answer, behind three functions.
//! Stage 1 interprets no AML, so each answers what an empty namespace does:
//! no GPE is the namespace's to run, and no embedded-controller query is
//! served. An interpreter replaces these bodies and nothing else: the event
//! loop, its guards, its counts and the controller's transport stay as they
//! are.

/// How a GPE's status is cleared against the method that serves it: an edge
/// GPE's before the method runs, so an edge during it is not lost; a level
/// GPE's after, so the source the method quiets does not raise it again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trigger {
    Edge,
    Level,
}

/// What the namespace does with an event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Disposition {
    /// Nothing serves it.
    Unserved,
}

/// A GPE the namespace runs, and how.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Gpe {
    pub trigger: Trigger,
    pub disposition: Disposition,
}

/// The GPEs the namespace has methods for (`_Lxx`, `_Exx`), enabled at start.
pub fn runtime_gpes() -> Vec<u16> {
    Vec::new()
}

/// A GPE [`runtime_gpes`] named, fired.
pub fn gpe(n: u16) -> Gpe {
    unreachable!("aml: GPE {n:#x} fired, and stage 1 enables none the namespace names")
}

/// An embedded-controller query, taken off the controller, run once the
/// drain that took it has ended: a query's method may itself talk to the
/// controller.
pub fn query(_q: u8) -> Disposition {
    Disposition::Unserved
}
