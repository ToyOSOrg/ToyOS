//! The performance envelope's registers, read on the CPU that runs this: the
//! read-back half of [`super::control_regs`]'s performance request.

use toyos_abi::perf::{CpuRegisters, PackageRegisters};
use toyos_perfstate::msr;

use super::cpu;

pub use super::control_regs::HwpDeclared as Declared;

/// The proof every read below needs, or why this machine has none.
pub fn declared() -> Result<Declared, &'static str> {
    Declared::ask().map_err(toyos_perfstate::Refusal::reason)
}

/// This CPU's own registers.
pub fn read_cpu(_: &Declared) -> CpuRegisters {
    CpuRegisters {
        pm_enable: cpu::rdmsr(msr::PM_ENABLE),
        hwp_capabilities: cpu::rdmsr(msr::HWP_CAPABILITIES),
        hwp_request: cpu::rdmsr(msr::HWP_REQUEST),
        energy_perf_bias: cpu::rdmsr(msr::ENERGY_PERF_BIAS),
        misc_enable: cpu::rdmsr(msr::MISC_ENABLE),
    }
}

/// The registers of the package this CPU is in.
pub fn read_package(_: &Declared) -> PackageRegisters {
    PackageRegisters {
        hwp_request_pkg: cpu::rdmsr(msr::HWP_REQUEST_PKG),
        platform_info: cpu::rdmsr(msr::PLATFORM_INFO),
        rapl_power_unit: cpu::rdmsr(msr::RAPL_POWER_UNIT),
        pkg_power_limit: cpu::rdmsr(msr::PKG_POWER_LIMIT),
        pkg_energy_status: cpu::rdmsr(msr::PKG_ENERGY_STATUS),
        package_therm_status: cpu::rdmsr(msr::PACKAGE_THERM_STATUS),
        temperature_target: cpu::rdmsr(msr::TEMPERATURE_TARGET),
    }
}
