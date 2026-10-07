//! The ACPI server's death, on a boot that does not start it: this job claims
//! the fixed hardware, which puts the machine in ACPI mode, hands the claim to
//! `/system/bin/acpiserver`, and kills it once it has armed. The kernel's
//! release of the claim writes `ACPI_DISABLE` and says what `SCI_EN` then
//! reads, which the `acpi_server_death` metal row judges; this asserts only
//! that the server armed and died.

use std::io::{BufRead, BufReader};
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};

use toyos::endow::{Endowments, DEV_PREFIX, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos::Device;
use toyos_abi::syscall::DeviceType;

const SERVER: &str = "/system/bin/acpiserver";

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a device-minting capability");
    let claim: Device = cap.claim(DeviceType::Acpi).expect("acpi_release: the fixed hardware's claim");
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
