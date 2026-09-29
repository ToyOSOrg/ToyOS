//! One read of a `perf-state` claim: every register it answers, one line for
//! the package and one per CPU, checked against the request the kernel
//! declares for that CPU (`toyos_perfstate`).

use toyos::Device;
use toyos_abi::perf::{answer_len, CpuRegisters, PackageRegisters};
use toyos_abi::syscall::{self, SyscallError};
use toyos_perfstate::{HwpRequest, TURBO_DISABLE};

/// Prints the answer, and names the first register that is not the
/// declaration's. The turbo bit is printed and not checked: the kernel
/// does not declare it.
pub fn read_back(claim: &Device) -> Result<(), String> {
    let cpus = syscall::cpu_count() as usize;
    let mut buf = vec![0u8; answer_len(cpus)];
    let n = claim.read(&mut buf).map_err(|e: SyscallError| format!("the read was refused: {e:?}"))?;
    if n != buf.len() {
        return Err(format!("the read answered {n} bytes, and {cpus} CPUs are {}", buf.len()));
    }
    let pkg = PackageRegisters::read_from(&buf).expect("the buffer holds the package record");
    println!(
        "pkg hwp_request_pkg={:#010x} platform_info={:#018x} package_therm_status={:#x}",
        pkg.hwp_request_pkg, pkg.platform_info, pkg.package_therm_status,
    );
    check("pkg", "hwp_request_pkg", pkg.hwp_request_pkg, toyos_perfstate::HWP_REQUEST_PKG)?;
    for cpu in 0..cpus {
        let regs = CpuRegisters::read_from(&buf[answer_len(cpu)..])
            .expect("the buffer holds every CPU's record");
        println!(
            "cpu{cpu} hardware_id={} pm_enable={} hwp_request={:#010x} {:?} epb={} turbo={} \
             hwp_capabilities={:#010x}",
            regs.hardware_id,
            regs.pm_enable,
            regs.hwp_request,
            HwpRequest::of(regs.hwp_request),
            regs.energy_perf_bias,
            if regs.misc_enable & TURBO_DISABLE == 0 { "on" } else { "off" },
            regs.hwp_capabilities,
        );
        let who = format!("cpu{cpu}");
        let declared = toyos_perfstate::hwp_request(regs.hwp_capabilities, pkg.platform_info);
        check(&who, "pm_enable", regs.pm_enable, toyos_perfstate::PM_ENABLE)?;
        check(&who, "hwp_request", regs.hwp_request, declared)?;
        check(&who, "epb", regs.energy_perf_bias, toyos_perfstate::ENERGY_PERF_BIAS)?;
    }
    Ok(())
}

fn check(who: &str, name: &str, holds: u64, declared: u64) -> Result<(), String> {
    if holds == declared {
        Ok(())
    } else {
        Err(format!("{who} holds {name}={holds:#x}, and the declaration is {declared:#x}"))
    }
}
