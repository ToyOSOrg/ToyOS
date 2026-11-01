//! `/system/bin/acpiserver`'s lines in the boot's log, for whoever reads one:
//! the server writes each through the constant its readers match on.

#![no_std]

/// The server's count of the embedded controller's queries, written once a
/// count interval where a count moved: after its count of SCIs, and before
/// each query number with how often it was taken.
pub const QUERIES_COUNTED: &str = "embedded controller queries taken: ";

/// The server's word on the embedded controller, once, after the tables'
/// load: [`CONTROLLER_SERVED`] and its GPE and ports where it serves one,
/// [`CONTROLLER_NONE`] and why where it serves none.
pub const CONTROLLER_SERVED: &str = "acpiserver: embedded controller on GPE ";
pub const CONTROLLER_NONE: &str = "acpiserver: embedded controller none: ";

/// The server's census of the processors' idle states, written once their
/// `_CST`s are handed to the kernel: how many declarations it kept, the
/// states each held, and what it withheld.
pub const IDLE_CENSUS: &str = "acpiserver: the processors' idle states: ";

/// The server's census of the batteries and adapters it found, written once
/// at the end of its start where a battery is present, after their first
/// reading.
pub const POWER_SOURCES: &str = "acpiserver: power sources: ";

/// A present battery's static information, once after the controller is
/// armed: `battery <n> of <count>`, then its numbers.
pub const BATTERY_INFO: &str = "battery information: ";

/// A battery's reading, on its first and wherever its whole percent or state
/// or the adapters' moved: `battery <n> of <count>: <percent>%`, its
/// remaining and full capacity, its state, rate, voltage and the power it
/// moves in mW, then the adapters' state.
pub const BATTERY_READ: &str = "battery read: ";

/// What opens a line that carries an address, a PCI function or a name the
/// firmware chose: this machine's own, and quoted in no record.
pub const OWN: &str = "acpiserver: (this machine's own, quoted in no record) ";

/// The first write to each register the kernel keeps for itself, which the
/// server answered as written: the write itself is the next line, under
/// [`OWN`].
pub const KEPT_WRITE: &str = "acpiserver: a SystemMemory write the kernel refused KernelRegister was not made, and the evaluation went on";

/// The first write the kernel made into each page of memory the firmware's
/// map lists nowhere or types memory-mapped I/O, then the page's type: the
/// write itself is the next line, under [`OWN`].
pub const FIRST_WRITE_IN_PAGE: &str = "acpiserver: a SystemMemory write made for the first time in a page of ";

/// The first write the kernel made into each function's configuration
/// space: the write itself is the next line, under [`OWN`].
pub const FIRST_WRITE_TO_FUNCTION: &str = "acpiserver: a PCI_Config write made for the first time to a function";
