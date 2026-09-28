//! What a read of a `perf-state` claim answers: one [`PackageRegisters`], then
//! one [`CpuRegisters`] per CPU in CPU order — the machine's whole CPU count,
//! or the read is refused whole with `ResourceExhausted`, and with `Io` when a
//! CPU did not answer within the kernel's bound. Every field is the
//! register's raw value, named by its x86-64 MSR; decoding is the reader's.

/// The package-wide registers, read on whichever CPU answered the read.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PackageRegisters {
    /// `IA32_HWP_REQUEST_PKG`, 0x772.
    pub hwp_request_pkg: u64,
    /// `MSR_PLATFORM_INFO`, 0xCE.
    pub platform_info: u64,
    /// `MSR_RAPL_POWER_UNIT`, 0x606.
    pub rapl_power_unit: u64,
    /// `MSR_PKG_POWER_LIMIT`, 0x610.
    pub pkg_power_limit: u64,
    /// `MSR_PKG_ENERGY_STATUS`, 0x611.
    pub pkg_energy_status: u64,
    /// `IA32_PACKAGE_THERM_STATUS`, 0x1B1.
    pub package_therm_status: u64,
    /// `MSR_TEMPERATURE_TARGET`, 0x1A2.
    pub temperature_target: u64,
}

/// One CPU's registers, read on that CPU.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuRegisters {
    /// `IA32_PM_ENABLE`, 0x770.
    pub pm_enable: u64,
    /// `IA32_HWP_CAPABILITIES`, 0x771.
    pub hwp_capabilities: u64,
    /// `IA32_HWP_REQUEST`, 0x774.
    pub hwp_request: u64,
    /// `IA32_ENERGY_PERF_BIAS`, 0x1B0.
    pub energy_perf_bias: u64,
    /// `IA32_MISC_ENABLE`, 0x1A0.
    pub misc_enable: u64,
}

// Every byte belongs to a field: both cross the boundary as bytes, so a gap
// would publish whatever the kernel stack held.
const _: () = assert!(core::mem::size_of::<PackageRegisters>() == 7 * 8);
const _: () = assert!(core::mem::size_of::<CpuRegisters>() == 5 * 8);

/// The bytes a read answers on a machine of `cpus` CPUs.
pub const fn answer_len(cpus: usize) -> usize {
    core::mem::size_of::<PackageRegisters>() + cpus * core::mem::size_of::<CpuRegisters>()
}

macro_rules! plain_bytes {
    ($($ty:ty),+) => {$(
        impl $ty {
            pub fn as_bytes(&self) -> &[u8] {
                // SAFETY: `self` is a valid `&Self`, readable for
                // `size_of::<Self>()` bytes, and the const asserts above prove
                // the `repr(C)` layout is all `u64` fields with no padding.
                unsafe {
                    core::slice::from_raw_parts(
                        self as *const Self as *const u8,
                        core::mem::size_of::<Self>(),
                    )
                }
            }

            /// The record at the start of `bytes`, or `None` if it is shorter.
            pub fn read_from(bytes: &[u8]) -> Option<Self> {
                let bytes = bytes.get(..core::mem::size_of::<Self>())?;
                let mut out = Self::default();
                // SAFETY: `out` is `size_of::<Self>()` bytes of `u64` fields,
                // every bit pattern of which is a valid value, and `bytes` is
                // exactly that long.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        bytes.as_ptr(),
                        &mut out as *mut Self as *mut u8,
                        bytes.len(),
                    );
                }
                Some(out)
            }
        }
    )+};
}

plain_bytes!(PackageRegisters, CpuRegisters);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_round_trips_through_its_bytes() {
        let cpu = CpuRegisters {
            pm_enable: 1,
            hwp_capabilities: 0x010d_182a,
            hwp_request: 0x8000_2a04,
            energy_perf_bias: 6,
            misc_enable: 0x0085_0089,
        };
        assert_eq!(CpuRegisters::read_from(cpu.as_bytes()), Some(cpu));
        assert_eq!(CpuRegisters::read_from(&cpu.as_bytes()[1..]), None);
        let pkg = PackageRegisters { pkg_power_limit: 0x0042_8200_00dd_8200, ..Default::default() };
        assert_eq!(PackageRegisters::read_from(pkg.as_bytes()), Some(pkg));
        assert_eq!(answer_len(8), 56 + 8 * 40);
    }
}
