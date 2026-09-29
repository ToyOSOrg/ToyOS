//! Every value here is synthetic: the shape `capture.sh` and `tcg.sh` write,
//! never a reading of the T14.

use super::*;

const VERSION: &str = "Linux version 6.8.0-142-generic (fixture) #142-Ubuntu SMP PREEMPT_DYNAMIC";
const CPUS: u32 = 2;

/// A capture as its parts, rendered into files by [`Fixture::files`].
struct Fixture {
    version: String,
    /// `cpu family`, `model`, `stepping` as cpuinfo reads them.
    cpuinfo: (u32, u32, u32),
    vendor_id: &'static str,
    microcode: &'static str,
    leaves: BTreeMap<(u32, u32), Regs>,
    /// Each MSR's value on each CPU.
    msrs: BTreeMap<u32, Vec<u64>>,
}

fn regs(eax: u32, ebx: u32, ecx: u32, edx: u32) -> Regs {
    Regs { eax, ebx, ecx, edx }
}

impl Fixture {
    /// A Tiger Lake: family 6, model 0x8c, stepping 1, microcode 0xbe.
    fn t14() -> Fixture {
        let mut leaves: BTreeMap<(u32, u32), Regs> =
            LEAVES.iter().map(|&k| (k, Regs::default())).collect();
        // "Genu" "ntel" "ineI" in EBX, ECX, EDX.
        leaves.insert((0, 0), regs(0x1b, 0x756e_6547, 0x6c65_746e, 0x4965_6e69));
        leaves.insert((1, 0), regs(0x0008_06c1, 0x11, 0x22, 0x33));
        leaves.insert((7, 0), regs(0, 0x44, 0x55, ARCH_CAPABILITIES_BIT | 1 << 26));
        leaves.insert((7, 2), regs(0, 0, 0, 0x66));
        leaves.insert((0x8000_0000, 0), regs(0x8000_0008, 0, 0, 0));
        leaves.insert((0x8000_0008, 0), regs(0x77, 0x88, 0x99, 0xaa));
        leaves.insert((0x8000_0021, 0), regs(0xbb, 0, 0, 0));
        let each = |v: u64| vec![v; CPUS as usize];
        let msrs = BTreeMap::from([
            (IA32_BIOS_SIGN_ID, each(0xbe_0000_0000)),
            (IA32_SPEC_CTRL, vec![0x400, 0x480]),
            (IA32_ARCH_CAPABILITIES, each(0x0c00_0000)),
            (IA32_MCU_OPT_CTRL, each(0)),
        ]);
        Fixture {
            version: VERSION.into(),
            cpuinfo: (6, 140, 1),
            vendor_id: "GenuineIntel",
            microcode: "0xbe",
            leaves,
            msrs,
        }
    }

    fn files(&self) -> BTreeMap<&'static str, String> {
        let vulnerabilities = [
            "gather_data_sampling:Mitigation: fixture",
            "meltdown:Not affected",
        ]
        .map(|l| format!("{VULNERABILITY}{l}\n"))
        .concat();
        let (family, model, stepping) = self.cpuinfo;
        let cpuinfo: String = (0..CPUS)
            .map(|c| {
                format!(
                    "processor\t: {c}\nvendor_id\t: {}\ncpu family\t: {family}\nmodel\t\t: {model}\n\
                     model name\t: fixture\nstepping\t: {stepping}\nmicrocode\t: {}\nflags\t\t: fpu\n\n",
                    self.vendor_id, self.microcode
                )
            })
            .collect();
        let cpuid: String = self
            .leaves
            .iter()
            .map(|(&(l, s), r)| {
                format!(
                    "{l:08x} {s:08x} {:08x} {:08x} {:08x} {:08x}\n",
                    r.eax, r.ebx, r.ecx, r.edx
                )
            })
            .collect();
        let msr: String = self
            .msrs
            .iter()
            .flat_map(|(m, per_cpu)| {
                per_cpu
                    .iter()
                    .enumerate()
                    .map(move |(c, v)| format!("{m:08x} {c} {v:016x}\n"))
            })
            .collect();
        BTreeMap::from([
            ("version.txt", format!("{}\n", self.version)),
            (
                "uname.txt",
                format!("Linux t14 {KERNEL} #142-Ubuntu x86_64 GNU/Linux\n"),
            ),
            (
                "cmdline.txt",
                format!("BOOT_IMAGE=/vmlinuz-{KERNEL} ro quiet splash\n"),
            ),
            (
                "packages.txt",
                format!("linux-image-{KERNEL}\t{PACKAGES}\nlinux-modules-{KERNEL}\t{PACKAGES}\n"),
            ),
            (
                "config-sha256.txt",
                format!("{CONFIG_SHA256}  /boot/config-{KERNEL}\n"),
            ),
            (
                "config-hardening.txt",
                "528:CONFIG_RANDOMIZE_BASE=y\n".into(),
            ),
            ("mmap_rnd_bits.txt", "vm.mmap_rnd_bits = 32\n".into()),
            ("vulnerabilities.txt", vulnerabilities),
            ("cpuinfo.txt", cpuinfo),
            (
                "kernel-log.txt",
                "Spectre V1 : Mitigation: fixture\n".into(),
            ),
            ("dmi.txt", "/sys/class/dmi/id/bios_version:fixture\n".into()),
            ("cpuid.txt", cpuid),
            ("msr.txt", msr),
        ])
    }

    fn parse(&self) -> Result<T14, String> {
        parse(&self.files())
    }
}

fn parse(files: &BTreeMap<&'static str, String>) -> Result<T14, String> {
    T14::parse(&|n| files.get(n).cloned())
}

fn refused(r: Result<T14, String>, naming: &str) {
    match r {
        Ok(t) => panic!("a capture that should be refused for {naming:?} reads: {t:?}"),
        Err(why) => assert!(why.contains(naming), "refused for {why:?}, not {naming:?}"),
    }
}

#[test]
fn a_capture_reads_into_the_facts_s1_takes() {
    let t = Fixture::t14().parse().unwrap();
    assert_eq!(
        t.facts,
        Facts {
            vendor: *b"GenuineIntel",
            family: 6,
            model: 0x8c,
            stepping: 1,
            microcode: 0xbe,
            leaf_1: regs(0x0008_06c1, 0x11, 0x22, 0x33),
            leaf_7_0: regs(0, 0x44, 0x55, ARCH_CAPABILITIES_BIT | 1 << 26),
            leaf_7_2: regs(0, 0, 0, 0x66),
            leaf_8000_0008_ebx: 0x88,
            leaf_8000_0021_eax: 0,
            arch_capabilities: 0x0c00_0000,
        }
    );
    assert_eq!(t.version, VERSION);
    assert_eq!(t.mmap_rnd_bits, 32);
    assert_eq!(t.lines["gather_data_sampling"], "Mitigation: fixture");
    assert_eq!(t.lines["meltdown"], "Not affected");
    assert_eq!(
        t.msrs[&IA32_SPEC_CTRL],
        BTreeMap::from([(0, 0x400), (1, 0x480)])
    );
}

#[test]
fn a_capture_missing_any_file_is_refused_by_its_name() {
    for name in T14_FILES {
        let mut files = Fixture::t14().files();
        files.remove(name);
        refused(parse(&files), name);
    }
}

#[test]
fn a_capture_of_another_kernel_package_or_config_is_refused() {
    let mut fx = Fixture::t14();
    fx.version = VERSION.replace("142", "141");
    refused(fx.parse(), "version.txt");
    for (file, from, to) in [
        ("packages.txt", "142.142\n", "142.141\n"),
        ("config-sha256.txt", "3b85", "3b86"),
    ] {
        let mut files = Fixture::t14().files();
        let text = files[file].replacen(from, to, 1);
        files.insert(file, text);
        refused(parse(&files), file);
    }
}

#[test]
fn an_extended_leaf_reads_as_zero_beyond_the_highest_and_as_itself_within_it() {
    let mut fx = Fixture::t14();
    fx.leaves
        .insert((0x8000_0000, 0), regs(0x8000_0007, 0, 0, 0));
    let f = fx.parse().unwrap().facts;
    assert_eq!((f.leaf_8000_0008_ebx, f.leaf_8000_0021_eax), (0, 0));
    fx.leaves
        .insert((0x8000_0000, 0), regs(0x8000_0021, 0, 0, 0));
    let f = fx.parse().unwrap().facts;
    assert_eq!((f.leaf_8000_0008_ebx, f.leaf_8000_0021_eax), (0x88, 0xbb));
}

#[test]
fn arch_capabilities_is_read_only_where_cpuid_enumerates_it() {
    let mut fx = Fixture::t14();
    fx.leaves.insert((7, 0), regs(0, 0x44, 0x55, 1 << 26));
    assert_eq!(fx.parse().unwrap().facts.arch_capabilities, 0);
}

#[test]
fn an_msr_the_cpu_enumerates_and_the_capture_lacks_is_refused() {
    let mut fx = Fixture::t14();
    fx.msrs
        .insert(IA32_ARCH_CAPABILITIES, vec![0x0c00_0080; CPUS as usize]);
    refused(fx.parse(), "0x122");
    fx.msrs.insert(IA32_TSX_CTRL, vec![3; CPUS as usize]);
    fx.parse().unwrap();

    let mut fx = Fixture::t14();
    fx.leaves.insert(
        (7, 0),
        regs(0, 0, 0, ARCH_CAPABILITIES_BIT | 1 << 11 | 1 << 13),
    );
    refused(fx.parse(), "0x10f");
    fx.leaves
        .insert((7, 0), regs(0, 0, 0, ARCH_CAPABILITIES_BIT | 1 << 11));
    fx.parse().unwrap();
}

#[test]
fn an_msr_read_on_too_few_cpus_or_disagreeing_where_it_is_one_value_is_refused() {
    let mut fx = Fixture::t14();
    fx.msrs.insert(IA32_MCU_OPT_CTRL, vec![0]);
    refused(fx.parse(), "0x123 read on CPUs {0}");
    let mut fx = Fixture::t14();
    fx.msrs
        .insert(IA32_ARCH_CAPABILITIES, vec![0x0c00_0000, 0x0d00_0000]);
    refused(fx.parse(), "MSR 0x10a: CPU 1");
}

#[test]
fn a_microcode_revision_the_msr_and_cpuinfo_disagree_on_is_refused() {
    let mut fx = Fixture::t14();
    fx.microcode = "0xbf";
    refused(fx.parse(), "IA32_BIOS_SIGN_ID");
}

#[test]
fn a_cpuid_decoding_linux_disagrees_with_is_refused() {
    let mut fx = Fixture::t14();
    fx.cpuinfo = (6, 141, 1);
    refused(fx.parse(), "model");
    let mut fx = Fixture::t14();
    fx.vendor_id = "AuthenticAMD";
    refused(fx.parse(), "CPUID.0");
}

#[test]
fn an_extended_family_and_model_decode_as_linux_decodes_them() {
    let mut fx = Fixture::t14();
    // "Auth" "cAMD" "enti" in EBX, ECX, EDX; base family 0xf, extended model 6.
    fx.leaves
        .insert((0, 0), regs(0xd, 0x6874_7541, 0x444d_4163, 0x6974_6e65));
    fx.leaves.insert((1, 0), regs(0x0006_0fb1, 0, 0, 0));
    fx.vendor_id = "AuthenticAMD";
    fx.cpuinfo = (15, 107, 1);
    let f = fx.parse().unwrap().facts;
    assert_eq!(
        (&f.vendor, f.family, f.model, f.stepping),
        (b"AuthenticAMD", 15, 107, 1)
    );
    fx.leaves.insert((1, 0), regs(0x00a5_0f00, 0, 0, 0));
    fx.cpuinfo = (25, 80, 0);
    fx.parse().unwrap();
}

fn tcg(console: &str, qemu: &str) -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        (
            "qemu-version.txt",
            format!("{qemu}\nCopyright (c) the QEMU Project developers\n"),
        ),
        ("console.txt", console.to_string()),
    ])
}

fn console(version: &str, names: &[&str]) -> String {
    let mut c = format!(
        "[    0.000000] {VERSION}\r\n[    1.000000] Run /init as init process\r\n{version}\r\n"
    );
    for n in names {
        c += &format!("{VULNERABILITY}{n}:Vulnerable\r\n");
    }
    c + "[    2.000000] reboot: Power down\r\n"
}

fn parse_tcg(files: &BTreeMap<&'static str, String>) -> Result<Tcg, String> {
    Tcg::parse(&|n| files.get(n).cloned(), &Fixture::t14().parse().unwrap())
}

#[test]
fn a_tcg_console_reads_the_lines_its_init_printed() {
    let qemu = format!("QEMU emulator version {}", qemu_pin());
    let t = parse_tcg(&tcg(
        &console(VERSION, &["gather_data_sampling", "meltdown"]),
        &qemu,
    ))
    .unwrap();
    assert_eq!(t.lines["meltdown"], "Vulnerable");
    assert_eq!(t.lines.len(), 2);
}

#[test]
fn a_tcg_capture_of_another_kernel_qemu_or_file_set_is_refused() {
    let qemu = format!("QEMU emulator version {}", qemu_pin());
    let both = ["gather_data_sampling", "meltdown"];
    let other = VERSION.replace("fixture", "another build");
    for (files, naming) in [
        (tcg(&console(&other, &both), &qemu), "/proc/version"),
        (
            tcg(&console(VERSION, &both), "QEMU emulator version 8.2.2"),
            "qemu-version.txt",
        ),
        (
            tcg(&console(VERSION, &["meltdown"]), &qemu),
            "vulnerabilities",
        ),
    ] {
        match parse_tcg(&files) {
            Ok(t) => panic!("read {t:?}, which should be refused for {naming:?}"),
            Err(why) => assert!(why.contains(naming), "refused for {why:?}, not {naming:?}"),
        }
    }
    for name in TCG_FILES {
        let mut files = tcg(&console(VERSION, &both), &qemu);
        files.remove(name);
        assert!(parse_tcg(&files).unwrap_err().contains(name));
    }
}
