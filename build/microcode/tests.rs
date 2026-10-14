//! The pinned files split into the patches each CPU is handed, and the lists
//! of the patches an image carries.

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
/// hold, and a QEMU image carries none.
#[test]
fn every_listed_patch_is_one_the_pinned_files_hold() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(root_files(root, Machines::Qemu).is_empty());
    for machines in [Machines::Benches, Machines::Release] {
        let files = root_files(root, machines);
        let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["share/microcode/intel/06-8c-01/80", "share/microcode/amd/17-60-01"], "{machines:?}");
    }
}
