//! `toyfetch`: what the system it runs on says of itself, beside its logo.
//!
//! Every fact comes through `sysinfo` and needs no authority a process does not
//! already hold; a fact the system does not answer is left out, never guessed.

mod render;

use std::path::Path;

use render::{Cpu, Facts, Memory};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

fn main() {
    let system = System::new_with_specifics(
        RefreshKind::nothing()
            .with_memory(MemoryRefreshKind::nothing().with_ram())
            .with_cpu(CpuRefreshKind::nothing()),
    );
    let cpus = system.cpus();
    let facts = Facts {
        os: System::long_os_version(),
        kernel: System::kernel_version().map(|_| System::kernel_long_version()),
        arch: System::cpu_arch(),
        cpu: cpus.first().map(|cpu| Cpu {
            brand: Some(cpu.brand().trim().to_owned()).filter(|brand| !brand.is_empty()),
            count: cpus.len(),
        }),
        memory: (system.total_memory() > 0).then(|| Memory {
            used: system.used_memory(),
            total: system.total_memory(),
        }),
        uptime: Some(System::uptime()).filter(|&secs| secs > 0),
        shell: std::env::var("SHELL").ok().map(|shell| {
            Path::new(&shell)
                .file_name()
                .map_or(shell.clone(), |name| name.to_string_lossy().into_owned())
        }),
        terminal: std::env::var("TERM_PROGRAM")
            .or_else(|_| std::env::var("TERM"))
            .ok(),
    };
    print!(
        "{}",
        render::render(&facts, render::logo_for(&System::distribution_id()))
    );
}
