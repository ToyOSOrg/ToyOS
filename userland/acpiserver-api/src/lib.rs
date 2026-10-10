//! `/system/bin/acpiserver`'s lines in the boot's log, for whoever reads one:
//! the server writes each through the constant its readers match on.

#![no_std]

/// The server's count of the embedded controller's queries, written once a
/// count interval where a count moved: after its count of SCIs, and before
/// each query number with how often it was taken.
pub const QUERIES_COUNTED: &str = "embedded controller queries taken: ";

/// A present battery's static information, once after the tables' load:
/// `battery <n> of <count>`, then its numbers.
pub const BATTERY_INFO: &str = "battery information: ";

/// A battery's reading, on its first and wherever its whole percent or state
/// or the adapters' moved: `battery <n> of <count>: <percent>%`, its
/// remaining and full capacity, its state, rate, voltage and the power it
/// moves in mW, then the adapters' state.
pub const BATTERY_READ: &str = "battery read: ";

/// No battery is read on this machine, and why.
pub const NO_BATTERY: &str = "no battery: ";
