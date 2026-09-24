//! A GPT partition as a claimable device, judged off the disks.
//!
//! The guest (`tests/toyos-rust-tests/src/bin/partition_claimant.rs`) claims
//! partitions of disks this file crafted and asserts every refusal the ABI
//! promises. What it cannot judge is what its writes did to the disks — that is
//! the claim in question — so the verdict is read here, after the guest has
//! gone, off the images:
//!
//! - every byte of the NVMe disk outside the target and DATA is the byte this
//!   file wrote: the primary and backup tables, both neighbours, the granted,
//!   misaligned and twin partitions, and the gaps;
//! - both neighbours are FAT32 volumes `toyos-fat32-check` (fatgen103's rules)
//!   has nothing to say about — the neighbour after the target begins at the
//!   block after its last, so a write one past the end lands in its boot
//!   sector;
//! - every block of the target is the pattern the guest wrote there;
//! - the `/home` file written between the target's transfers reads back
//!   through the host's own bcachefs reader;
//! - after a departure, the stick holds what the guest wrote again.
//!
//! The partition ranges are UEFI 2.10 §5.3.3's, as the `gpt` crate — not the
//! kernel's parser — laid them out, and each partition's unique GUID is fixed
//! here, where the table and the `system.toml` naming it are both written.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use super::qemu::{self, BootOptions, QemuInstance, Staged};

/// Mirrored in the guest binary: the idle ROOT slot the guest writes whole.
const TARGET: &str = "7B1D4A3C-2E5F-4C8A-9D6B-0A1F2E3D4C5B";
/// Mirrored in the guest and in `tests/partclaimcase/system.toml`.
const GRANTED: &str = "A94F0E6D-3B2C-4E1A-8C7D-6E5F4A3B2C1D";
/// Mirrored: a partition whose length is not whole 4 KiB blocks.
const MISALIGNED: &str = "3E8A1C5F-7D2B-4F60-9A1E-5C4B3D2E1F07";
/// Mirrored: the unique GUID the NVMe disk and the USB stick both carry.
const TWIN: &str = "6D2F9B41-8C3E-4A57-B1D0-2E4F6A8C0B13";
/// Mirrored: DATA, which the kernel mounts at `/home`.
const DATA: &str = "E3A7C5D9-1B2F-4E6A-8D0C-9F7B5A3E1C24";
/// Mirrored: the two partitions of the stick whose device leaves.
const DEPARTING: &str = "1F3E5D7C-9B2A-4C6E-8F01-A3B5C7D9E2F4";
const STAYING: &str = "2A4C6E80-1B3D-4F57-9E6A-C8D0B2F4A6E1";

/// The two FAT32 neighbours' type, and every other test partition's.
const NEIGHBOUR_TYPE: &str = "5C3E8F21-9A4B-4D7E-8F10-2B3C4D5E6F70";
const PLAIN_TYPE: &str = "0FC63DAF-8483-4772-8E79-3D69D8477DE4";

/// Mirrored: the target's length in blocks.
const TARGET_BLOCKS: u64 = 2048;
/// The granted partition's length in blocks.
const GRANTED_BLOCKS: u64 = 256;
const HOME_FILE: &str = "home/partclaim-interleaved.bin";
const HOME_CHUNK: usize = 32 * 1024;
const PAST_END: &[u8; 16] = b"TOYOS-PAST-END\0\0";
/// The claims the guest expects refused: four mounted partitions, the one init
/// granted test-runner, a twin, a misaligned one, an absent GUID, the zero GUID, three
/// claims carrying selector words their class does not read, the target a
/// second time, and the target while a child holds it.
const REFUSALS: usize = 14;
/// The ESP, the log partition, ROOT and DATA.
const MOUNTED: usize = 4;
const BLOCK: u64 = 4096;
const MIB: u64 = 1024 * 1024;

/// The config whose one difference from the test estate is the granted row.
const CONFIG: &str = "tests/partclaimcase";

/// Mirrored in the guest: what block `n` of the target holds once it is done.
fn pattern(n: u64) -> Vec<u8> {
    let mut block = vec![0u8; BLOCK as usize];
    for (i, byte) in block.iter_mut().enumerate() {
        *byte = (n as usize).wrapping_mul(31).wrapping_add(i) as u8;
    }
    block[..8].copy_from_slice(&n.to_le_bytes());
    block[8..24].copy_from_slice(b"TOYOS-PARTCLAIM\0");
    block
}

/// Mirrored in the guest: block `n` of a departure partition, `which` being
/// `D` or `S`.
fn departure_block(which: u8, n: u64) -> Vec<u8> {
    let mut block = vec![which; BLOCK as usize];
    block[..8].copy_from_slice(&n.to_le_bytes());
    block[8..24].copy_from_slice(b"TOYOS-DEPARTURE\0");
    block
}

/// Where one partition landed, in bytes.
#[derive(Clone, Copy, Debug)]
struct Span {
    start: u64,
    len: u64,
}

impl Span {
    fn end(self) -> u64 {
        self.start + self.len
    }
    fn of(self, disk: &[u8]) -> &[u8] {
        &disk[self.start as usize..self.end() as usize]
    }
}

struct Layout {
    before: Span,
    target: Span,
    after: Span,
    data: Span,
}

/// The claims, refusals, idle ROOT slot, releases and neighbours, on the
/// machine this suite flashes plus a second USB stick for the twin GUID.
pub fn partition_claim(
    _test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let config = super::compile::repo_root().join(CONFIG);
    let boot_image = super::lane::dir().join("partclaim-boot.img");
    std::fs::write(&boot_image, qemu::build_boot_image(&config, c_bins, rust_bins, &[]))
        .map_err(|e| format!("write the boot image: {e}"))?;
    let [esp, log, root] = boot_stick_guids(&boot_image)?;
    let nvme = super::lane::dir().join("partclaim-disk.img");
    let layout = craft_nvme(&nvme)?;
    let stick = super::lane::dir().join("partclaim-stick.img");
    let (stick_bytes, _) = qemu::Profile::UsbDisk.usb_disk().expect("UsbDisk declares a disk");
    let twin_on_stick = craft_stick(&stick, stick_bytes, &[("twin", MIB, TWIN)])?[0];

    // The premises, checked rather than assumed: a neighbour that did not
    // begin where the target ends would let a write past the end land in a
    // gap this test does not look at as hard.
    if layout.before.end() != layout.target.start || layout.target.end() != layout.after.start {
        return Err(format!("the neighbours do not touch the target: {:?}", (
            layout.before, layout.target, layout.after
        )));
    }
    if layout.target.len != TARGET_BLOCKS * BLOCK {
        return Err(format!("the target is {} bytes, not {TARGET_BLOCKS} blocks", layout.target.len));
    }
    let before = std::fs::read(&nvme).map_err(|e| format!("read the crafted disk: {e}"))?;
    for (what, span) in [("first", layout.before), ("second", layout.after)] {
        let complaints = toyos_fat32_check::check(span.of(&before));
        if !complaints.is_empty() {
            return Err(format!(
                "the {what} neighbour is not a clean FAT32 before any guest ran:\n{}",
                toyos_fat32_check::describe(&complaints)
            ));
        }
    }
    let twin_before = read_span(&stick, twin_on_stick)?;

    let mut qemu = QemuInstance::boot_with_options(
        &config,
        c_bins,
        rust_bins,
        BootOptions {
            profile: qemu::Profile::UsbDisk,
            boot_image: Some(Staged::Pristine(boot_image.clone())),
            nvme_image: Some(nvme.clone()),
            usb_images: vec![stick.clone()],
            ..Default::default()
        },
    );
    let boot = qemu.boot_log().to_string();
    no_panic("booting the claim disks", &boot)?;
    if boot.contains("are a tmpfs") {
        return Err(format!("/home fell back to tmpfs, so nothing shares the disk:\n{boot}"));
    }
    // init names what it could not mint; a grant it refused would make the
    // guest's endowment about nothing.
    let grant = format!("part:{GRANTED}");
    if let Some(line) = boot.lines().find(|l| l.contains("init: test-runner:") && l.contains(&grant)) {
        return Err(format!("init did not grant test-runner its partition: {line}\n{boot}"));
    }

    // A guest that failed is judged off the disk all the same: what its failure
    // did to the neighbours is the half of the verdict it cannot give itself.
    let run = format!("test_rs_partition_claimant main {esp} {log} {root}");
    let result = qemu.run_test(&run, Duration::from_secs(180));
    let guest = guest_verdict(&result, REFUSALS).and_then(|kernel| main_kernel_lines(&kernel));
    let tail = shut_down(qemu);
    no_panic("on the way down", &tail)?;

    let after = std::fs::read(&nvme).map_err(|e| format!("read the disk back: {e}"))?;
    let neighbours = neighbours_untouched(&layout, &before, &after);
    if guest.is_err() || !neighbours.is_empty() {
        return Err(format!(
            "guest: {}\nneighbours, off the image: {}",
            guest.err().unwrap_or_else(|| "every assertion held".to_string()),
            if neighbours.is_empty() { "untouched".to_string() } else { neighbours.join("\n") }
        ));
    }
    if read_span(&stick, twin_on_stick)? != twin_before {
        return Err("the twin partition on the stick changed, and nothing may write it".into());
    }
    target_holds_the_pattern(&layout, &after)?;
    home_file_reads_back(&nvme)?;

    for path in [&nvme, &stick, &boot_image] {
        let _ = std::fs::remove_file(path);
    }
    eprintln!(
        "  [partclaim] {REFUSALS} claims refused by name; the idle ROOT slot's {TARGET_BLOCKS} \
         blocks written and read back through the claim; released by close and by its holder's \
         death; both FAT32 neighbours untouched byte for byte and clean to fatgen103; /home \
         intact"
    );
    Ok(())
}

/// The two exits of a claim that gets no answer: a disk that does not answer a
/// read of its table refuses the claim rather than resolving it on the disks
/// that did, and a transfer every attempt of which is refused on its budget
/// ends at the deadman with the device's word.
pub fn partition_claim_gives_up(
    _test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let config = super::compile::repo_root().join(CONFIG);
    let nvme = super::lane::dir().join("partclaim-gives-up.img");
    let cases: [(&'static [&'static str], &str, usize, &[&str]); 2] = [
        (
            &["partclaim-table-unanswered"],
            "unanswered",
            1,
            &[" did not answer a read of LBA 0 while looking for "],
        ),
        (
            &["fsync-budget-spent", "fsync-deadman-now"],
            "deadman",
            0,
            &[
                "partclaim: a write still refused after 1 attempt(s)",
                "partclaim: a read still refused after 1 attempt(s)",
            ],
        ),
    ];
    for (params, role, refusals, wants) in cases {
        craft_nvme(&nvme)?;
        let mut qemu = QemuInstance::boot_with_options(
            &config,
            c_bins,
            rust_bins,
            BootOptions {
                profile: qemu::Profile::Metal,
                nvme_image: Some(nvme.clone()),
                kernel_params: params,
                ..Default::default()
            },
        );
        let boot = qemu.boot_log().to_string();
        no_panic(role, &boot)?;
        let result =
            qemu.run_test(&format!("test_rs_partition_claimant {role}"), Duration::from_secs(180));
        let kernel = guest_verdict(&result, refusals).map_err(|e| format!("{role}: {e}"))?;
        for want in wants {
            if !kernel.contains(want) {
                return Err(format!("{role}: the kernel never said {want:?}:\n{kernel}"));
            }
        }
        let tail = shut_down(qemu);
        no_panic(role, &tail)?;
        for want in wants {
            let line = kernel.lines().find(|l| l.contains(want)).unwrap_or_default();
            eprintln!("  [partclaim] {role}: {}", line.trim());
        }
    }
    let _ = std::fs::remove_file(&nvme);
    Ok(())
}

/// Two claims on a USB stick whose device leaves owing a flush of one of
/// theirs and is moved to another port by the host, as a reset moved T14 run
/// 79's stick: each claim's fsync answers for its own writes.
///
/// The machine boots off NVMe, so the stick's only writer is the guest and the
/// write `usb-transport-break-owed` breaks is the guest's second write to
/// `DEPARTING` — the first owed one. The flush asked first after the return is
/// `STAYING`'s, which lost nothing; the one after it is `DEPARTING`'s, which
/// did.
pub fn partition_claim_departure(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    const MOVE_NOW: &str = "usb-reset-moves: move the device now";
    const PARAMS: &[&str] = &["usb-transport-break-owed", "usb-reset-moves"];
    const CAME_BACK: &str = "usb-storage: disk 0 came back on port 3 slot ";
    const TOLD: &str = "writes a process's partition claim made before its disk came back owing \
        a flush may not have survived";
    let profile = qemu::Profile::NvmeBootUsbDisk;
    let (bytes, _) = profile.usb_disk().expect("NvmeBootUsbDisk declares a disk");
    let stick = super::lane::dir().join("partclaim-departure.img");
    let spans =
        craft_stick(&stick, bytes, &[("departing", MIB, DEPARTING), ("staying", MIB, STAYING)])?;
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            profile,
            qmp: true,
            smp: 2,
            kernel_params: PARAMS,
            usb_images: vec![stick.clone()],
            ..Default::default()
        },
    );
    let boot = qemu.boot_log().to_string();
    no_panic("booting off NVMe beside the stick", &boot)?;
    let moved = stick.clone();
    let result = qemu.run_test_hooked(
        "test_rs_partition_claimant departure",
        Duration::from_secs(240),
        MOVE_NOW,
        move |socket| {
            let mut devices = qemu::QmpDevices::open(socket);
            devices.del(&qemu::usb_device_id(0));
            devices.blockdev_add_again("moved", &moved);
            devices.add(
                "usb-storage",
                "xhci.0",
                "movedstick",
                &[("drive", "moved"), ("port", "3"), ("serial", qemu::DATA_STICK_SERIAL)],
            );
        },
    );
    let kernel = guest_verdict(&result, 1)?;
    for want in [MOVE_NOW, CAME_BACK, TOLD] {
        if !kernel.contains(want) {
            return Err(format!("the kernel never said {want:?}:\n{kernel}"));
        }
    }
    let told = kernel.matches(TOLD).count();
    if told != 1 {
        return Err(format!("{told} flushes were told of the loss, and one claim wrote it:\n{kernel}"));
    }
    let tail = shut_down(qemu);
    no_panic("on the way down", &tail)?;

    let [departing, staying] = [spans[0], spans[1]];
    let got = read_span(&stick, Span { start: departing.start, len: 2 * BLOCK })?;
    if got != [departure_block(b'D', 0), departure_block(b'D', 1)].concat() {
        return Err("the departing partition does not hold the blocks written again".into());
    }
    if read_span(&stick, Span { start: staying.start, len: BLOCK })? != departure_block(b'S', 0) {
        return Err("the staying partition does not hold its block".into());
    }
    let _ = std::fs::remove_file(&stick);
    eprintln!(
        "  [partclaim] the stick left owing the departing claim's write and came back on port 3; \
         the staying claim's fsync answered Ok, the departing claim's Io once and Ok after, and \
         the stick holds both claims' blocks"
    );
    Ok(())
}

/// What the guest said: exit 0, `refusals` refusals said by name — an exit
/// code alone is also what a binary that asserted nothing leaves — and the
/// kernel's log while it ran, which is returned.
fn guest_verdict(result: &qemu::TestResult, refusals: usize) -> Result<String, String> {
    let kernel = format!("{}{}", result.before, result.serial);
    if result.exit_code != Some(0) {
        return Err(format!(
            "the guest failed:\n{}\nkernel log while it ran:\n{kernel}",
            result.stdout
        ));
    }
    let said = result.stdout.lines().filter(|l| l.contains(" refused with ")).count();
    if said != refusals || !result.stdout.contains("partition_claimant: PASS") {
        return Err(format!(
            "the guest exited 0 having said {said} of its {refusals} refusals:\n{}",
            result.stdout
        ));
    }
    Ok(kernel)
}

/// The kernel's own account of the main run: its hold named once for each
/// mounted partition refused, the twin and the misaligned partition refused
/// by name, and not one line for a transfer refused past the end — a caller
/// can ask at syscall rate.
fn main_kernel_lines(kernel: &str) -> Result<(), String> {
    let held = kernel.matches("is held by the kernel").count();
    if held != MOUNTED {
        return Err(format!(
            "the kernel named its own hold {held} times for {MOUNTED} mounted partitions:\n{kernel}"
        ));
    }
    for want in [format!("partclaim: {TWIN} is on device "), format!("partclaim: {MISALIGNED} is at ")]
    {
        if !kernel.contains(&want) {
            return Err(format!("the kernel never said {want:?}:\n{kernel}"));
        }
    }
    if let Some(line) = kernel.lines().find(|l| l.contains("block(s) at") && l.contains("refusing")) {
        return Err(format!("a caller's refused transfer wrote the kernel's log: {line:?}"));
    }
    Ok(())
}

fn no_panic(when: &str, log: &str) -> Result<(), String> {
    for bad in ["PANIC:", "panicked at"] {
        if log.contains(bad) {
            return Err(format!("{bad:?} {when}\n{log}"));
        }
    }
    Ok(())
}

/// `run shutdown`, and what the console said on the way down.
fn shut_down(mut qemu: QemuInstance) -> String {
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    qemu.drain_serial(Duration::from_secs(30))
}

/// The unique GUIDs of the ESP, the log partition and ROOT the image builder
/// drew for this boot image: the partitions the guest must find the kernel
/// holding.
fn boot_stick_guids(image: &Path) -> Result<[String; 3], String> {
    let disk = gpt::GptConfig::new()
        .writable(false)
        .logical_block_size(gpt::disk::LogicalBlockSize::Lb512)
        .open(image)
        .map_err(|e| format!("the boot image has no readable GPT: {e}"))?;
    let one = |kind: &str, what: &str| -> Result<String, String> {
        let found: Vec<_> = disk
            .partitions()
            .values()
            .filter(|p| p.part_type_guid.guid.eq_ignore_ascii_case(kind))
            .collect();
        match found.as_slice() {
            [part] => Ok(part.part_guid.to_string().to_uppercase()),
            _ => Err(format!("the boot image has {} of {what}, expected one", found.len())),
        }
    };
    // ROOT's type is read with the kernel's parser: the `gpt` crate answers the
    // all-zero GUID for a type its own table does not name.
    let bytes = std::fs::read(image).map_err(|e| format!("read the boot image: {e}"))?;
    let blank = toyos_gpt::Partition {
        index: 0,
        type_guid: toyos_gpt::Guid::ZERO,
        unique_guid: toyos_gpt::Guid::ZERO,
        first_lba: 0,
        last_lba: 0,
    };
    let mut found = [blank; 2];
    let scan = toyos_gpt::locate_type(
        &mut super::volumes::ImageSectors { bytes: &bytes },
        toyos_gpt::Guid::TOYOS_ROOT,
        &mut found,
    )
    .map_err(|e| format!("the boot image's table: {e:?}"))?;
    if scan.matched != 1 {
        return Err(format!("the boot image has {} of ROOT, expected one", scan.matched));
    }
    Ok([
        one(gpt::partition_types::EFI.guid, "ESP")?,
        one(gpt::partition_types::BASIC.guid, "log partition")?,
        found[0].unique_guid.to_string(),
    ])
}

/// Everything that says a byte outside the target and DATA moved: the first
/// such byte, and each neighbour fatgen103's rules have something to say about.
fn neighbours_untouched(layout: &Layout, before: &[u8], after: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    if before.len() != after.len() {
        found.push(format!("the disk changed size: {} -> {}", before.len(), after.len()));
        return found;
    }
    let owned = [layout.target, layout.data];
    let mut at = 0u64;
    while at < before.len() as u64 {
        if let Some(span) = owned.iter().find(|s| s.start <= at && at < s.end()) {
            at = span.end();
            continue;
        }
        let i = at as usize;
        if before[i] != after[i] {
            let block = at / BLOCK;
            let past_end = after[(block * BLOCK) as usize..][..PAST_END.len()] == *PAST_END;
            found.push(format!(
                "byte {at} (device block {block}) changed outside the claimed partition{}",
                if past_end { " — it holds the write the guest made past the end" } else { "" }
            ));
            break;
        }
        at += 1;
    }
    for (what, span) in [("first", layout.before), ("second", layout.after)] {
        let complaints = toyos_fat32_check::check(span.of(after));
        if !complaints.is_empty() {
            found.push(format!(
                "the {what} neighbour is not the FAT32 it was:\n{}",
                toyos_fat32_check::describe(&complaints)
            ));
        }
    }
    found
}

/// Every block of the target is the pattern the guest wrote there.
fn target_holds_the_pattern(layout: &Layout, after: &[u8]) -> Result<(), String> {
    let target = layout.target.of(after);
    for n in 0..TARGET_BLOCKS {
        let got = &target[(n * BLOCK) as usize..((n + 1) * BLOCK) as usize];
        if got != pattern(n) {
            return Err(format!("target block {n} is not what the guest wrote there"));
        }
    }
    Ok(())
}

/// The `/home` file the guest wrote between the target's transfers, through
/// the host's bcachefs reader over a plain seek-and-read of the image.
fn home_file_reads_back(image: &Path) -> Result<(), String> {
    let io = super::storage::FileBlocks::open(image)?;
    let fs = bcachefs::Mounted::<_, bcachefs::ReadOnly>::open(io)
        .map_err(|e| format!("DATA does not mount on the host: {e:?}"))?;
    let got = fs.read_file(HOME_FILE).map_err(|e| format!("reading {HOME_FILE}: {e:?}"))?;
    let runs = TARGET_BLOCKS.div_ceil(32);
    let want: Vec<u8> = (0..runs)
        .filter(|run| run % 8 == 0)
        .flat_map(|run| (0..HOME_CHUNK).map(move |i| (run as usize ^ i) as u8))
        .collect();
    if got != want {
        return Err(format!(
            "{HOME_FILE} is {} bytes off the image against the {} the guest wrote",
            got.len(),
            want.len()
        ));
    }
    Ok(())
}

/// `span`'s bytes of the file at `path`, without reading the rest of a sparse
/// stick.
fn read_span(path: &Path, span: Span) -> Result<Vec<u8>, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    file.seek(SeekFrom::Start(span.start)).map_err(|e| format!("seek: {e}"))?;
    let mut buf = vec![0u8; span.len as usize];
    file.read_exact(&mut buf).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(buf)
}

/// One partition a crafted table carries: its name, length in bytes, type and
/// unique GUID.
type Part = (&'static str, u64, &'static str, &'static str);

/// A disk of `bytes` at `path` holding `parts` in order, each on a 1 MiB
/// boundary, with the `gpt` crate writing both copies of the table; the disk,
/// and each partition's span in the same order.
fn table(path: &Path, bytes: u64, parts: &[Part]) -> Result<(Box<dyn gpt::DiskDevice>, Vec<Span>), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("create the disk: {e}"))?;
    file.set_len(bytes).map_err(|e| format!("size the disk: {e}"))?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("open the disk: {e}"))?;
    let mbr =
        gpt::mbr::ProtectiveMBR::with_lb_size(u32::try_from(bytes / 512 - 1).unwrap_or(0xFFFF_FFFF));
    mbr.overwrite_lba0(&mut file).map_err(|e| format!("protective MBR: {e}"))?;
    let mut gdisk = gpt::GptConfig::default()
        .initialized(false)
        .writable(true)
        .logical_block_size(gpt::disk::LogicalBlockSize::Lb512)
        .create_from_device(Box::new(file), None)
        .map_err(|e| format!("create the table: {e}"))?;
    gdisk
        .update_partitions(std::collections::BTreeMap::new())
        .map_err(|e| format!("initialise the table: {e}"))?;
    let mut ids = Vec::new();
    for &(name, len, guid, _) in parts {
        let ty = gpt::partition_types::Type { guid, os: gpt::partition_types::OperatingSystem::None };
        let id = gdisk.add_partition(name, len, ty, 0, Some(MIB / 512));
        ids.push(id.map_err(|e| format!("add {name}: {e}"))?);
    }
    // Each unique GUID fixed where the table is written, so the `part:` row or
    // the guest constant that names it names this partition and no other.
    let mut fixed = gdisk.partitions().clone();
    for (id, &(name, _, _, unique)) in ids.iter().zip(parts) {
        let part = fixed.get_mut(id).ok_or_else(|| format!("{name} was just added"))?;
        part.part_guid = uuid::Uuid::parse_str(unique).map_err(|e| format!("{unique}: {e}"))?;
    }
    gdisk.update_partitions(fixed).map_err(|e| format!("fix the unique GUIDs: {e}"))?;
    let lb = gpt::disk::LogicalBlockSize::Lb512;
    let mut spans = Vec::new();
    for id in &ids {
        let part = gdisk.partitions().get(id).ok_or("a partition that was just added")?;
        spans.push(Span {
            start: part.bytes_start(lb).map_err(|e| format!("start: {e}"))?,
            len: part.bytes_len(lb).map_err(|e| format!("length: {e}"))?,
        });
    }
    let device = gdisk.write().map_err(|e| format!("write the table: {e}"))?;
    Ok((device, spans))
}

/// The NVMe disk: a FAT32 neighbour, the idle ROOT slot, a FAT32 neighbour
/// touching it, the partition init grants, a partition that is not whole
/// blocks, the twin, and a DATA the kernel formats and mounts at `/home`.
fn craft_nvme(path: &Path) -> Result<Layout, String> {
    const FAT_BYTES: u64 = 34 * MIB;
    const DATA_BYTES: u64 = 96 * MIB;
    let parts: [Part; 7] = [
        ("neighbour before", FAT_BYTES, NEIGHBOUR_TYPE, "11111111-2222-4333-8444-555555555501"),
        ("idle ROOT slot", TARGET_BLOCKS * BLOCK, toyos_gpt::Guid::TOYOS_ROOT_TEXT, TARGET),
        ("neighbour after", FAT_BYTES, NEIGHBOUR_TYPE, "11111111-2222-4333-8444-555555555502"),
        ("granted", GRANTED_BLOCKS * BLOCK, PLAIN_TYPE, GRANTED),
        ("misaligned", MIB + 512, PLAIN_TYPE, MISALIGNED),
        ("twin", MIB, PLAIN_TYPE, TWIN),
        ("ToyOS data", DATA_BYTES, toyos_gpt::Guid::TOYOS_DATA_TEXT, DATA),
    ];
    let total = MIB + parts.iter().map(|p| p.1.next_multiple_of(MIB)).sum::<u64>() + 2 * MIB;
    let (mut device, spans) = table(path, total, &parts)?;
    let layout = Layout { before: spans[0], target: spans[1], after: spans[2], data: spans[6] };
    for (label, span) in [("PC-BEFORE", layout.before), ("PC-AFTER", layout.after)] {
        let volume = fat32(span.len as usize, label)?;
        device.seek(SeekFrom::Start(span.start)).map_err(|e| format!("seek: {e}"))?;
        device.write_all(&volume).map_err(|e| format!("write {label}: {e}"))?;
    }
    // The designation: the kernel formats a DATA only on this consent.
    let mut stamp = [0u8; BLOCK as usize];
    stamp[..bcachefs::DESIGNATION_MAGIC.len()].copy_from_slice(&bcachefs::DESIGNATION_MAGIC);
    let at = bcachefs::DESIGNATION_BLOCKS_OFFSET;
    stamp[at..at + 8].copy_from_slice(&(layout.data.len / BLOCK).to_le_bytes());
    device.seek(SeekFrom::Start(layout.data.start)).map_err(|e| format!("seek: {e}"))?;
    device.write_all(&stamp).map_err(|e| format!("stamp DATA: {e}"))?;
    device.flush().map_err(|e| format!("flush the disk: {e}"))?;
    Ok(layout)
}

/// A USB stick of `bytes` carrying `parts`, each a name, a length and its
/// unique GUID; their spans.
fn craft_stick(
    path: &Path,
    bytes: u64,
    parts: &[(&'static str, u64, &'static str)],
) -> Result<Vec<Span>, String> {
    let parts: Vec<Part> =
        parts.iter().map(|&(name, len, unique)| (name, len, PLAIN_TYPE, unique)).collect();
    let (mut device, spans) = table(path, bytes, &parts)?;
    device.flush().map_err(|e| format!("flush the stick: {e}"))?;
    Ok(spans)
}

/// A FAT32 volume of `bytes` holding one file, so the check has a directory
/// entry and a cluster chain to judge and not only a boot sector.
fn fat32(bytes: usize, label: &str) -> Result<Vec<u8>, String> {
    let mut volume = vec![0u8; bytes];
    let mut name = [b' '; 11];
    name[..label.len()].copy_from_slice(label.as_bytes());
    fatfs::format_volume(
        std::io::Cursor::new(&mut volume),
        fatfs::FormatVolumeOptions::new().fat_type(fatfs::FatType::Fat32).volume_label(name),
    )
    .map_err(|e| format!("format {label}: {e}"))?;
    {
        let fs = fatfs::FileSystem::new(std::io::Cursor::new(&mut volume), fatfs::FsOptions::new())
            .map_err(|e| format!("mount {label} on the host: {e}"))?;
        let mut file = fs.root_dir().create_file("NEIGHBOUR.TXT").map_err(|e| format!("{e}"))?;
        file.write_all(label.as_bytes()).map_err(|e| format!("{e}"))?;
        file.flush().map_err(|e| format!("{e}"))?;
        drop(file);
        // Counted before the unmount so FSInfo carries a free count, as every
        // writer that has finished with a volume leaves it.
        fs.stats().map_err(|e| format!("count {label}'s free clusters: {e}"))?;
        fs.unmount().map_err(|e| format!("unmount {label}: {e}"))?;
    }
    Ok(volume)
}
