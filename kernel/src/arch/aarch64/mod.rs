//! AArch64.
pub mod barrier {}
pub mod boot {}
pub mod cache {}
pub mod cpu {}
pub mod entry {}
pub mod fpu {}
pub mod hw {}
pub mod irqchip {}
pub mod keyboard_controller {}
pub mod iommu_unit {}
pub mod paging {}
pub mod percpu {}
pub mod rtc {}
pub mod smp {}
pub mod syscall {}
pub mod tlb {}
pub mod trap {}
pub mod watchdog {}
pub mod control_regs {}

/// The machine every program image this kernel loads must be built for.
pub const ELF_MACHINE: toyos_elf::Machine = toyos_elf::Machine::Aarch64;
