//! The processor's vendor and family, which the processor objects' rules key
//! on: whose `_OSC` interface they speak (`osc`) and what their idle states
//! ask of OSPM (`cst`).

use toyos_abi::ProcessorId;

/// Whose processor it is, by CPUID leaf 0's vendor string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Vendor {
    Intel,
    Amd,
    Hygon,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cpu {
    pub vendor: Vendor,
    /// The DisplayFamily, 0 where the processor has no CPUID.
    pub family: u32,
}

impl Cpu {
    pub fn here() -> Self {
        Self::of(toyos_abi::processor_id())
    }

    /// `id`'s vendor, and its family by the DisplayFamily rule (Intel SDM
    /// Vol. 2A, "CPUID"; AMD APM Vol. 3, CPUID Fn0000_0001_EAX): the base
    /// family, plus the extended family where the base is Fh.
    pub fn of(id: Option<ProcessorId>) -> Self {
        let Some(ProcessorId { vendor, signature }) = id else { return Self { vendor: Vendor::Other, family: 0 } };
        let base = signature >> 8 & 0xF;
        let family = if base == 0xF { base + (signature >> 20 & 0xFF) } else { base };
        let vendor = match &vendor {
            b"GenuineIntel" => Vendor::Intel,
            b"AuthenticAMD" => Vendor::Amd,
            b"HygonGenuine" => Vendor::Hygon,
            _ => Vendor::Other,
        };
        Self { vendor, family }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_family_adds_the_extended_family_only_to_a_base_of_f() {
        let of = |vendor: &[u8; 12], signature| Cpu::of(Some(ProcessorId { vendor: *vendor, signature }));
        assert_eq!(of(b"AuthenticAMD", 0x0086_0F01), Cpu { vendor: Vendor::Amd, family: 0x17 });
        assert_eq!(of(b"AuthenticAMD", 0x0073_0F01), Cpu { vendor: Vendor::Amd, family: 0x16 });
        assert_eq!(of(b"AuthenticAMD", 0x00B4_0F00), Cpu { vendor: Vendor::Amd, family: 0x1A });
        assert_eq!(of(b"HygonGenuine", 0x0090_0F02), Cpu { vendor: Vendor::Hygon, family: 0x18 });
        assert_eq!(of(b"GenuineIntel", 0x0008_06C1), Cpu { vendor: Vendor::Intel, family: 6 });
        assert_eq!(of(b"CentaurHauls", 0x00F0_06F2), Cpu { vendor: Vendor::Other, family: 6 });
        assert_eq!(Cpu::of(None), Cpu { vendor: Vendor::Other, family: 0 });
    }
}
