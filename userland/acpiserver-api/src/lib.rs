//! `/system/bin/acpiserver`'s lines in the boot's log, for whoever reads one:
//! the server writes each through the constant its readers match on.

#![no_std]

/// The server's count of the embedded controller's queries, written once a
/// count interval where a count moved: after its count of SCIs, and before
/// each query number with how often it was taken.
pub const QUERIES_COUNTED: &str = "embedded controller queries taken: ";
