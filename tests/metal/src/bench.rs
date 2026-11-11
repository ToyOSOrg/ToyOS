//! The metal benches: how a boot names the one it ran on, and what each has
//! that a row may need.

#![forbid(unsafe_code)]

use std::fmt;

use toyos_build::bootlog;

/// A metal bench.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bench {
    T14,
    Amd,
}

/// Hardware a row drives or reads, which not every bench has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    /// The Intel I219 Ethernet controller, the one wired card ToyOS drives.
    I219,
    /// An embedded controller the machine's ACPI tables define, and the
    /// battery read through it.
    EmbeddedController,
    /// Intel's hardware-managed P-states, `IA32_PM_ENABLE`.
    Hwp,
    /// A Tiger Lake integrated GPU.
    TigerLakeGpu,
    /// An Intel VT-d remapping unit.
    Vtd,
    /// An AMD-Vi unit.
    AmdVi,
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::I219 => "an I219",
            Self::EmbeddedController => "an embedded controller its tables define",
            Self::Hwp => "hardware-managed P-states",
            Self::TigerLakeGpu => "a Tiger Lake GPU",
            Self::Vtd => "a VT-d unit",
            Self::AmdVi => "an AMD-Vi unit",
        })
    }
}

/// How the kernel's record of a CPU's microcode opens, ahead of the CPU's
/// number, its vendor and [`SIGNATURE`].
const MICROCODE: &str = "microcode: cpu";
const SIGNATURE: &str = " signature ";

impl Bench {
    pub const ALL: [Bench; 2] = [Bench::T14, Bench::Amd];

    /// `--metal-bench`'s word for this bench.
    pub const fn word(self) -> &'static str {
        match self {
            Self::T14 => "t14",
            Self::Amd => "amd",
        }
    }

    /// The bench's machine as `ssh` reaches it, `<user>@<machine>`: the
    /// `--host` a run of the bench hands `toyos-metal`, and for the T14 the
    /// machine `toyos-metal` reaches without one.
    pub const fn host(self) -> &'static str {
        match self {
            Self::T14 => "toyos@bench-t14",
            Self::Amd => "toyos@bench-amd",
        }
    }

    /// The bench [`Self::word`] names.
    pub fn parse(word: &str) -> Result<Self, String> {
        Self::ALL.into_iter().find(|bench| bench.word() == word).ok_or_else(|| {
            let words: Vec<&str> = Self::ALL.iter().map(|bench| bench.word()).collect();
            format!("{word:?} names no bench; the benches are {}", words.join(" and "))
        })
    }

    /// `CPUID.01H:EAX` of this bench's processors. No two benches share one;
    /// two that did would need a second fact of the boot to tell them by.
    pub const fn signature(self) -> u32 {
        match self {
            Self::T14 => 0x0008_06c1,
            Self::Amd => 0x0086_0f01,
        }
    }

    pub const fn has(self, device: Device) -> bool {
        match device {
            Device::I219 | Device::EmbeddedController | Device::Hwp | Device::TigerLakeGpu | Device::Vtd => {
                matches!(self, Self::T14)
            }
            Device::AmdVi => matches!(self, Self::Amd),
        }
    }

    /// The bench a boot ran on, by its own account: the processor signature
    /// in the kernel's first microcode record. `log` is the boot's log, whole
    /// or its kernel records alone.
    pub fn of_log(log: &str) -> Result<Self, String> {
        let said = log
            .lines()
            .filter_map(bootlog::message)
            .find(|message| message.starts_with(MICROCODE))
            .ok_or_else(|| format!("names no bench: the kernel wrote no `{MICROCODE}` record"))?;
        let signature = said
            .split_once(SIGNATURE)
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .and_then(|word| u32::from_str_radix(word.trim_start_matches("0x"), 16).ok())
            .ok_or_else(|| format!("names no bench: {said:?} carries no signature"))?;
        Self::ALL.into_iter().find(|bench| bench.signature() == signature).ok_or_else(|| {
            format!("names no bench: no bench's processors have the signature {signature:#x}")
        })
    }
}

impl fmt::Display for Bench {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::T14 => "the T14",
            Self::Amd => "the AMD bench",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each bench's first kernel record, as its own boot wrote it: the AMD
    /// bench's before the log's clock is set.
    const T14: &str = "[2026-11-11 04:48:09 10.298 cpu0 kernel] microcode: cpu0 Intel signature 0x806c1 platform 7 found 0xbe current 0xbe running 0xbe\n";
    const AMD: &str = "[---------- -------- --.--- cpu0 kernel] microcode: cpu0 Amd signature 0x860f01 found 0x8600106 loaded 0x860010d running 0x860010d\n";

    #[test]
    fn a_boot_names_its_bench_in_its_first_microcode_record() {
        assert_eq!(Bench::of_log(T14), Ok(Bench::T14));
        assert_eq!(Bench::of_log(AMD), Ok(Bench::Amd));
        // The first record, whatever a later CPU or a program says.
        let later = format!("{AMD}[ 0.100 job] microcode: cpu9 Intel signature 0x806c1\n{T14}");
        assert_eq!(Bench::of_log(&later), Ok(Bench::Amd));
    }

    #[test]
    fn a_log_that_names_no_bench_is_refused_saying_why() {
        let none = "[2026-11-11 04:48:09 10.298 cpu0 kernel] Boot: complete (1165ms)\n";
        assert!(Bench::of_log(none).unwrap_err().contains("wrote no `microcode: cpu` record"));
        let guest = T14.replace("0x806c1", "0x60fb1");
        assert!(Bench::of_log(&guest).unwrap_err().contains("the signature 0x60fb1"));
        let cut = "[ 0.001 cpu0 kernel] microcode: cpu0 Intel\n";
        assert!(Bench::of_log(cut).unwrap_err().contains("carries no signature"));
    }

    #[test]
    fn a_bench_is_named_by_one_word_one_signature_and_one_machine() {
        let machine = |bench: Bench| crate::host_of(bench.host()).expect("a host `ssh` reads").1;
        for bench in Bench::ALL {
            assert_eq!(Bench::parse(bench.word()), Ok(bench));
            let same = Bench::ALL.iter().filter(|other| other.signature() == bench.signature()).count();
            assert_eq!(same, 1, "{bench} shares its signature");
            let same = Bench::ALL.iter().filter(|other| machine(**other) == machine(bench)).count();
            assert_eq!(same, 1, "{bench} shares its machine");
        }
        assert!(Bench::parse("T14").unwrap_err().contains("t14 and amd"));
    }
}
