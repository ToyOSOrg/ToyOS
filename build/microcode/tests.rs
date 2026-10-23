//! The pinned files split into the patches each CPU is handed, and the lists
//! of the patches an image carries.

use toyos_microcode::Cpu;

use super::*;

/// The patches the pinned files split into, which a build that cannot split
/// them refuses by name.
fn split_ok() -> BTreeMap<String, Patch<'static>> {
    split().unwrap_or_else(|refused| panic!("{refused:?}"))
}

#[test]
fn the_benches_patches_are_their_vendors_files_named_for_their_cpus() {
    let patches = split_ok();
    let t14 = &patches["intel/06-8c-01/80"];
    assert_eq!(t14.revision, 0xbe);
    assert!(t14.bytes == INTEL[0].1, "the T14's patch is not Intel's whole 06-8c-01");
    let bench = &patches["amd/17-60-01"];
    assert_eq!(bench.revision, 0x0860_010d);
    assert!(bench.bytes == &AMD[0].1[0x262c..0x262c + 3200], "the AMD bench's patch is not its section's MPB");
}

/// Every CPU the pinned files name: the T14's one platform; 06-cc-02's four
/// signatures, its header's repeated first in its extended table, on the
/// three platforms of flags 0x94; the seven signatures of AMD's Family 17h
/// table.
#[test]
fn every_cpu_a_pinned_file_names_has_a_patch() {
    let names: Vec<String> = split_ok().into_keys().collect();
    assert_eq!(names.len(), 1 + 12 + 7, "{names:?}");
    for name in ["intel/06-e5-02/04", "intel/06-e5-02/10", "intel/06-e5-02/80", "amd/17-68-01", "amd/17-a0-00"] {
        assert!(names.iter().any(|n| n == name), "no {name} in {names:?}");
    }
    assert!(!names.iter().any(|n| n.starts_with("intel/06-8c-01/") && n != "intel/06-8c-01/80"));
}

fn patch(revision: u32, bytes: &'static [u8]) -> Patch<'static> {
    Patch { revision, bytes }
}

fn named(patches: &[(&str, Patch<'static>)]) -> Result<BTreeMap<String, Patch<'static>>, Refused> {
    newest(patches.iter().map(|&(name, patch)| (name.to_string(), patch)))
}

#[test]
fn the_newest_patch_for_a_cpu_is_chosen_wherever_it_sits() {
    let (older, newer, other) = (patch(0xbd, b"older"), patch(0xbe, b"newer"), patch(0xc0, b"other"));
    for order in [[older, newer], [newer, older]] {
        let chosen = named(&[("t14", order[0]), ("other", other), ("t14", order[1])]).unwrap();
        assert_eq!(chosen["t14"], newer);
        assert_eq!(chosen["other"], other);
    }
}

#[test]
fn two_different_patches_at_a_cpus_newest_level_are_refused() {
    let tie = named(&[("t14", patch(0xbe, b"one")), ("t14", patch(0xbe, b"two"))]);
    assert_eq!(tie, Err(Refused::Tie { name: "t14".into(), revision: 0xbe }));
    // A newer one settles a tie below it, and one patch carried twice is one.
    let settled = named(&[("t14", patch(0xbd, b"one")), ("t14", patch(0xbd, b"two")), ("t14", patch(0xbe, b"new"))]);
    assert_eq!(settled.unwrap()["t14"], patch(0xbe, b"new"));
    let twice = named(&[("t14", patch(0xbe, b"same")), ("t14", patch(0xbe, b"same"))]);
    assert_eq!(twice.unwrap()["t14"], patch(0xbe, b"same"));
}

/// Every name `images/microcode.toml` lists is a patch the pinned files
/// hold, which `root_files` refuses the build on otherwise, and a QEMU image
/// carries none.
#[test]
fn every_listed_patch_is_one_the_pinned_files_hold() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(root_files(root, Machines::Qemu).is_empty());
    for machines in [Machines::Benches, Machines::Release] {
        assert!(!root_files(root, machines).is_empty(), "{machines:?}");
    }
}

#[test]
fn two_cpus_under_one_name_are_refused() {
    let t14 = Cpu::Intel { signature: 0x0008_06c1, platform: toyos_microcode::PlatformId::from_msr(7 << 50) };
    let typed = Cpu::Intel { signature: 0x0008_26c1, platform: toyos_microcode::PlatformId::from_msr(7 << 50) };
    let patch = patch(0xbe, b"t14");
    assert!(named_cpus([(t14, patch), (t14, patch)].into_iter()).is_ok(), "one CPU named twice");
    assert_eq!(
        named_cpus([(t14, patch), (typed, patch)].into_iter()).map(|_| ()),
        Err(Refused::Collision { name: "intel/06-8c-01/80".into() }),
    );
}

/// The loader's lookup over a ROOT the build writes: each bench's CPU gets
/// its own patch byte for byte, and a CPU the image carries none for is told
/// so by name rather than handed another's.
#[test]
fn the_loader_reads_each_benchs_patch_off_a_built_root() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = root_files(root, Machines::Benches);
    files.push(("etc/os-release".to_string(), b"release".to_vec()));
    let image = crate::image::create_root_image(&files, &[("bin/ls".to_string(), "/system/bin/toybox".to_string())], true);
    let patches = split_ok();
    let t14 = Cpu::Intel { signature: 0x0008_06c1, platform: toyos_microcode::PlatformId::from_msr(0x001c_0000_0000_0000) };
    let amd_bench = Cpu::Amd { signature: 0x0086_0f01 };
    // The loader's own composition: the CPU's path, read off ROOT's bytes.
    let lookup = |cpu: Cpu| bcachefs::engine::read_file(&image, cpu.path().to_string().as_bytes());
    for (cpu, name) in [(t14, "intel/06-8c-01/80"), (amd_bench, "amd/17-60-01")] {
        assert!(lookup(cpu).expect(name) == patches[name].bytes, "{name}");
    }
    assert_eq!(lookup(Cpu::Amd { signature: 0x00a0_0f11 }), Err(bcachefs::engine::Error::NotFound));
}
