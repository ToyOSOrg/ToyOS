//! The machines ToyOS runs on.
//!
//! Every target triple, QEMU binary, firmware image, guest CPU and accelerator
//! the build system and the harness name is a function of one [`Arch`]. Neither
//! architecture is the reference: a new question about a machine is a new
//! method here that both variants answer, or it does not compile.

use std::path::Path;

/// One instruction-set architecture ToyOS builds for and boots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Arch {
    X86_64,
    Aarch64,
}

/// How a guest's CPU is provided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accel {
    /// Linux's hypervisor, on a host of the guest's own architecture.
    Kvm,
    /// macOS's Hypervisor.framework, on a host of the guest's own architecture.
    Hvf,
    /// QEMU's emulator: every other host.
    Tcg,
}

impl Accel {
    /// The name `-accel` takes.
    pub const fn name(self) -> &'static str {
        match self {
            Accel::Kvm => "kvm",
            Accel::Hvf => "hvf",
            Accel::Tcg => "tcg",
        }
    }

    /// Whether the guest runs on the host's own CPU rather than an emulated one.
    pub const fn is_hardware(self) -> bool {
        !matches!(self, Accel::Tcg)
    }
}

impl Arch {
    pub const ALL: [Arch; 2] = [Arch::X86_64, Arch::Aarch64];

    /// The architecture this build system runs on, when ToyOS runs there too.
    pub const HOST: Option<Arch> = if cfg!(target_arch = "x86_64") {
        Some(Arch::X86_64)
    } else if cfg!(target_arch = "aarch64") {
        Some(Arch::Aarch64)
    } else {
        None
    };

    /// The name a command line spells it with, and Rust's `target_arch`.
    pub const fn name(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        }
    }

    pub fn parse(name: &str) -> Result<Arch, String> {
        Arch::ALL.into_iter().find(|arch| arch.name() == name).ok_or_else(|| {
            let known: Vec<&str> = Arch::ALL.iter().map(|a| a.name()).collect();
            format!("{name:?} is not an architecture ToyOS builds for; it builds for {}", known.join(" and "))
        })
    }

    /// ToyOS userland's target: the rust fork's.
    pub const fn userland(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64-unknown-toyos",
            Arch::Aarch64 => "aarch64-unknown-toyos",
        }
    }

    /// The kernel's bare-metal target: one without hardware float, so kernel
    /// code never touches the FP/SIMD registers that are the user's state.
    pub const fn kernel(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64-unknown-none",
            Arch::Aarch64 => "aarch64-unknown-none-softfloat",
        }
    }

    /// The UEFI loader's target.
    pub const fn loader(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64-unknown-uefi",
            Arch::Aarch64 => "aarch64-unknown-uefi",
        }
    }

    /// Where on the ESP firmware looks for a removable medium's loader: UEFI
    /// 2.11 §3.5.1.1 names one file per architecture.
    pub const fn removable_loader(self) -> &'static str {
        match self {
            Arch::X86_64 => "EFI/BOOT/BOOTx64.EFI",
            Arch::Aarch64 => "EFI/BOOT/BOOTAA64.EFI",
        }
    }

    /// The QEMU that emulates this machine.
    pub const fn qemu(self) -> &'static str {
        match self {
            Arch::X86_64 => "qemu-system-x86_64",
            Arch::Aarch64 => "qemu-system-aarch64",
        }
    }

    /// The firmware's code and variable-store images, relative to the
    /// repository root, pinned and hashed in `NOTICE`.
    pub const fn firmware(self) -> (&'static str, &'static str) {
        match self {
            Arch::X86_64 => ("ovmf/OVMF_CODE-pure-efi.fd", "ovmf/OVMF_VARS-pure-efi.fd"),
            Arch::Aarch64 => ("aavmf/AAVMF_CODE.fd", "aavmf/AAVMF_VARS.fd"),
        }
    }

    /// How this host provides a guest of this architecture: its own
    /// hypervisor when the host is the same architecture and will open it,
    /// and emulation otherwise.
    ///
    /// **Presence is not permission**, and `Path::exists` cannot tell the two
    /// apart. A GitHub runner ships `/dev/kvm` as `crw-rw---- root:kvm` with the
    /// build user outside the group, so a check on existence puts `-accel kvm`
    /// on every boot and every boot dies on `failed to initialize kvm:
    /// Permission denied` — a whole suite red for a reason no test names.
    /// Opening it is the question QEMU is about to ask.
    pub fn accel(self) -> Accel {
        if Arch::HOST != Some(self) {
            return Accel::Tcg;
        }
        if cfg!(target_os = "macos") {
            return Accel::Hvf;
        }
        let opens = cfg!(target_os = "linux")
            && std::fs::OpenOptions::new().read(true).write(true).open(Path::new("/dev/kvm")).is_ok();
        if opens {
            Accel::Kvm
        } else {
            Accel::Tcg
        }
    }

    /// The CPU every guest of this architecture gets under `accel`.
    ///
    /// **One declaration, read by `cargo run` and by the harness both**, because
    /// the two drifted: the harness gained `+smep` and the interactive path did
    /// not, so the machine an owner looked at differed from the machine the
    /// suite judged in exactly the dimension the suite had been changed for.
    /// The emulated CPU carries the same features off a base model, because a
    /// TCG guest that withholds one is a feature this tree stops exercising.
    pub const fn cpu(self, accel: Accel) -> &'static str {
        match (self, accel.is_hardware()) {
            (Arch::X86_64, true) => "host,+rdrand,+smap,+fsgsbase,+x2apic,+smep",
            (Arch::X86_64, false) => "qemu64,+rdrand,+smap,+fsgsbase,+x2apic,+smep",
            (Arch::Aarch64, true) => "host",
            (Arch::Aarch64, false) => "max",
        }
    }

    /// The machine's three targets: userland, kernel, loader.
    pub const fn targets(self) -> [&'static str; 3] {
        [self.userland(), self.kernel(), self.loader()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_parses_back_to_its_arch_and_nothing_else_does() {
        for arch in Arch::ALL {
            assert_eq!(Arch::parse(arch.name()), Ok(arch));
        }
        let refusal = Arch::parse("riscv64").unwrap_err();
        assert!(refusal.contains("x86_64") && refusal.contains("aarch64"), "{refusal}");
    }

    #[test]
    fn every_target_names_its_own_architecture() {
        for arch in Arch::ALL {
            for triple in arch.targets() {
                assert!(triple.starts_with(&format!("{}-", arch.name())), "{triple}");
            }
            assert!(arch.qemu().ends_with(arch.name()));
        }
    }

    #[test]
    fn a_guest_of_another_architecture_is_emulated() {
        for arch in Arch::ALL {
            if Arch::HOST != Some(arch) {
                assert_eq!(arch.accel(), Accel::Tcg, "{arch:?}");
            }
        }
    }
}
