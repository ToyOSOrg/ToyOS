extern crate std;

use std::string::ToString;

use super::*;

/// The T14's i5-1135G7, platform 7 as all eight of its CPUs read
/// `IA32_PLATFORM_ID`.
const T14: Cpu = Cpu::Intel { signature: 0x0008_06c1, platform: PlatformId::from_msr(0x001c_0000_0000_0000) };
/// The AMD bench's Ryzen 7 4800U.
const AMD_BENCH: Cpu = Cpu::Amd { signature: 0x0086_0f01 };

#[test]
fn the_benches_patches_are_where_their_vendors_name_them() {
    assert_eq!(T14.path().to_string(), "share/microcode/intel/06-8c-01/80");
    assert_eq!(AMD_BENCH.path().to_string(), "share/microcode/amd/17-60-01");
}

#[test]
fn intels_platform_is_its_flag_bit_and_nothing_else_in_the_msr() {
    assert_eq!(PlatformId::from_msr(!(7 << 50)), PlatformId(0));
    let names: std::vec::Vec<_> = PlatformId::all()
        .map(|platform| Cpu::Intel { signature: 0x0008_06c1, platform }.to_string())
        .collect();
    assert_eq!(names[0], "intel/06-8c-01/01");
    assert_eq!(names[4], "intel/06-8c-01/10");
    assert_eq!(names.len(), 8);
}

/// SDM Vol. 2A's Display Family and Model: the extended model for families
/// 6 and 0FH, the extended family for 0FH alone, and the processor type
/// (bits 13:12) in no name.
#[test]
fn intels_names_are_its_display_family_and_model() {
    let name = |signature| Cpu::Intel { signature, platform: PlatformId(0) }.to_string();
    assert_eq!(name(0x000e_0652), "intel/06-e5-02/01");
    assert_eq!(name(0x0000_06f6), "intel/06-0f-06/01");
    assert_eq!(name(0x0010_0f41), "intel/10-04-01/01");
    assert_eq!(name(0x00b0_0771), "intel/07-07-01/01", "neither extended field counts for family 7");
    assert_eq!(name(0x0008_26c1), name(0x0008_06c1));
}

/// APM Vol. 3's Family and Model: both extended fields count for base
/// family 0FH alone.
#[test]
fn amds_names_are_its_family_and_model() {
    let name = |signature| Cpu::Amd { signature }.to_string();
    assert_eq!(name(0x00a2_0f12), "amd/19-21-02");
    assert_eq!(name(0x00b4_0f40), "amd/1a-44-00");
    assert_eq!(name(0x0089_0e01), "amd/0e-00-01", "neither extended field counts below family 0FH");
}
