//! x86-64.

/// The machine the kernel image must be built for: the loader's own.
pub const ELF_MACHINE: toyos_elf::Machine = toyos_elf::Machine::X86_64;
