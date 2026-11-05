//! The ACPI server's death, on a boot that does not start it: this job claims
//! the fixed hardware from a thread held off the boot processor. The claim
//! puts the machine in ACPI mode by a write the asking CPU asked for (the T14
//! judge requires one off the boot processor; a guest's row does not). It hands the claim to
//! `/system/bin/acpiserver`, and kills it once it has declared the root
//! bridges' memory, which outlives it: the supervisor starts storage and the
//! PCI drivers on that declaration. The kernel's release of the claim writes
//! `ACPI_DISABLE` and says what `SCI_EN` then reads, which the
//! `acpi_server_death` metal row judges; this asserts only that the server
//! declared and died.

use std::io::{BufRead, BufReader};
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};

use toyos::endow::{Endowments, DEV_PREFIX, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos::Device;
use toyos_abi::syscall::DeviceType;

#[path = "../arch/cpu.rs"]
mod cpu;
#[path = "../off_the_boot_processor.rs"]
mod off_the_boot_processor;

const SERVER: &str = "/system/bin/acpiserver";

/// What the server says once the kernel holds its declaration.
const DECLARED: &str = "acpiserver: the root bridges' memory declared: ";

/// What it says instead where the kernel holds none of it.
const UNDECLARED: [&str; 3] = [
    "acpiserver: no root bridge declared",
    "acpiserver: the kernel refused the root bridges' memory",
    "acpiserver: the kernel already holds an answer",
];

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a device-minting capability");
    let (on, claim) = off_the_boot_processor::run(|| {
        (cpu::x2apic_id(), cap.claim::<Device>(DeviceType::Acpi).expect("acpi_release: the fixed hardware's claim"))
    });
    println!("acpi_release: claimed from the CPU of x2APIC id {on}");
    let mut server = Command::new(SERVER)
        .endow(&format!("{DEV_PREFIX}{}", DeviceType::Acpi.class_name()), claim.into())
        .stdout(Stdio::piped())
        .spawn()
        .expect("acpi_release: spawn the server");
    let said = BufReader::new(server.stdout.take().expect("the server's piped stdout"));
    // The server's own word: it panics by name on anything it cannot serve, and
    // a pipe that ends first is that. It arms before it declares.
    let declared = said
        .lines()
        .map(|line| line.expect("acpi_release: read the server's line"))
        .inspect(|line| println!("acpi_release: the server said: {line}"))
        .find(|line| UNDECLARED.iter().chain([&DECLARED]).any(|word| line.starts_with(word)))
        .expect("acpi_release: the server ended before it declared the root bridges' memory");
    assert!(declared.starts_with(DECLARED), "acpi_release: the server's declaration was not held: {declared}");
    server.kill().expect("acpi_release: kill the server");
    let status = server.wait().expect("acpi_release: wait for the server");
    assert!(!status.success(), "acpi_release: a killed server ended {status:?}");
    println!("acpi_release: the server declared ({declared}) and was killed");
}
