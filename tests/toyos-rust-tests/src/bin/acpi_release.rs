//! The ACPI server's death, on a boot that does not start it: this job claims
//! the fixed hardware from a thread off the boot processor, which puts the
//! machine in ACPI mode by a write another CPU asked for, hands the claim to
//! `/system/bin/acpiserver`, and kills it once it has armed. The kernel's
//! release of the claim writes `ACPI_DISABLE` and says what `SCI_EN` then
//! reads, which the `acpi_server_death` metal row judges; this asserts only
//! that the server armed and died.

use std::io::{BufRead, BufReader};
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;

use toyos::endow::{Endowments, DEV_PREFIX, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos::Device;
use toyos_abi::syscall::DeviceType;

#[path = "../arch/cpu.rs"]
mod cpu;

const SERVER: &str = "/system/bin/acpiserver";

/// Threads started in turn before one must have found itself off the boot
/// processor: a spawn goes to the least loaded CPU from a start that rotates.
const STARTS: usize = 64;

/// The claim, asked by a thread that read itself off the boot processor as
/// its first act: x2APIC id 0, which the judge holds against the kernel's own
/// `asked from`. The kernel moves no running thread, so the asker is that CPU
/// unless a preemption between the read and the kernel's lock queued the
/// thread behind another and an idle boot processor took it.
fn claim_off_the_boot_processor(cap: &SysCap) -> Device {
    for _ in 0..STARTS {
        let claimed = thread::scope(|threads| {
            let asker = threads.spawn(|| {
                let on = cpu::x2apic_id();
                (on != 0).then(|| (on, cap.claim::<Device>(DeviceType::Acpi).expect("acpi_release: the fixed hardware's claim")))
            });
            asker.join().expect("acpi_release: the claiming thread")
        });
        if let Some((on, claim)) = claimed {
            println!("acpi_release: claimed from the CPU of x2APIC id {on}");
            return claim;
        }
    }
    panic!("acpi_release: {STARTS} threads in a row started on the boot processor");
}

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a device-minting capability");
    let claim = claim_off_the_boot_processor(&cap);
    let mut server = Command::new(SERVER)
        .endow(&format!("{DEV_PREFIX}{}", DeviceType::Acpi.class_name()), claim.into_raw().0)
        .stdout(Stdio::piped())
        .spawn()
        .expect("acpi_release: spawn the server");
    let said = BufReader::new(server.stdout.take().expect("the server's piped stdout"));
    // The server's own word: it panics by name on anything it cannot serve, and
    // a pipe that ends first is that.
    let armed = said
        .lines()
        .map(|line| line.expect("acpi_release: read the server's line"))
        .inspect(|line| println!("acpi_release: the server said: {line}"))
        .find(|line| line.starts_with("acpiserver: armed: "))
        .expect("acpi_release: the server ended before it armed");
    server.kill().expect("acpi_release: kill the server");
    let status = server.wait().expect("acpi_release: wait for the server");
    assert!(!status.success(), "acpi_release: a killed server ended {status:?}");
    println!("acpi_release: the server armed ({armed}) and was killed");
}
