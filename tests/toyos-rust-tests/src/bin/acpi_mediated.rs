//! The `acpi` claim's mediated access, asked as its holder asks: what the
//! kernel reads and writes for it, what it refuses and by which name, and the
//! firmware's Global Lock taken, found owned and given back.
//!
//! Run on a boot that starts no ACPI server (`tests/acpicase`) and whose
//! kernel leaves the i8042 to a claim (`i8042-withheld`), so its row is
//! another claim's; on a guest: every write here that must be refused is one
//! a kernel that made it would make for real — to RAM, to the tables, to
//! COM1, to `PM1a_CNT`, to a function's configuration space — and the
//! firmware's side of the lock is staged on the FACS itself, which only a
//! machine whose firmware is not using it can take. Each address is found as
//! a holder finds it, from the RSDP the claim's description names.
//!
//! **A byte for `SMI_CMD` is a call into the firmware** ([`firmware`]), which
//! this guest's chipset model answers by keeping the byte, and by leaving
//! ACPI mode on the one the FADT names for that: the port read back is
//! QEMU's word for which bytes the kernel wrote, and `SCI_EN` its word that
//! the kernel's own command was not among them. The real machine's firmware
//! runs a handler on every byte, so none is written to it by a probe.
//!
//! The power-off is the kernel's with the sleep type a holder supplies, once
//! under its claim. On this boot nothing has, so first a shutdown is refused
//! and the machine goes on.
//!
//! Then a child is handed the claim, supplies a sleep type this guest does
//! not power off on, is refused a second, takes the lock and exits with it:
//! the claim binds to one process for that process's life, so the parent
//! claims only after, and reads the lock word free.
//!
//! The parent, the next holder, is refused a word the register's field does
//! not hold, supplies this guest's own over the child's, and is refused a
//! second in its turn.
//!
//! Last it takes the lock once more and asks for the power-off holding it: a
//! stop ends no process, so the kernel finds a live holder's lock taken and
//! gives it back before the power-off owns the hardware, which
//! `acpi_mediated_access` reads in the kernel's own line; and the guest
//! powers off, which it does on the parent's sleep type and not the child's.
//!
//! **What a holder supplied stands once it is gone** ([`outlived`]), on a
//! boot of its own, since a power-off ends one: the boot whose ROOT carries
//! [`OUTLIVED`] has a child supply this guest's own sleep type and exit, and
//! the parent, which supplied none, ask for the power-off after the child's
//! claim and one more have been released.

use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos::{AsHandle, Device};
use toyos_abi::acpi::{pci_address, Access, AcpiInfo, Refused, Space, Width, UNLISTED};
use toyos_abi::counters::{Counter, RawRecord, Record};
use toyos_abi::syscall::{self, debug_action, DeviceType, SyscallError};
use toyos_abi::RawHandle;

#[path = "../arch/cpu.rs"]
mod cpu;

const SELF_PATH: &str = "/system/bin/test_rs_acpi_mediated";
const CLAIM_LABEL: &str = "acpi-claim";

/// The file whose presence on ROOT makes this boot [`outlived`]'s: a job is a
/// binary's name and takes no argument.
const OUTLIVED: &str = "/system/share/acpi_mediated_outlived";

/// `EFI_MEMORY_TYPE`s: what the kernel hands out as RAM, and ACPI's two.
const USABLE: [u8; 5] = [1, 2, 3, 4, 7];
const ACPI_RECLAIM: u8 = 9;
const ACPI_NVS: u8 = 10;

/// The liveness ceiling on a dead holder's claim coming back, as `isa_row`'s.
const RELEASED: Duration = Duration::from_secs(5);

struct Holder(Device);

impl Holder {
    fn handle(&self) -> RawHandle {
        self.0.as_handle()
    }

    fn ask(&self, mut access: Access) -> (Result<u64, Refused>, u8) {
        let made = syscall::acpi_access(self.handle(), &mut access).expect("acpi: a well-formed access on a bound claim");
        (made, access.memory_type)
    }

    fn read(&self, space: Space, address: u64, width: Width) -> Result<u64, Refused> {
        self.ask(Access::read(space, address, width)).0
    }

    fn write(&self, space: Space, address: u64, width: Width, value: u64) -> Result<(), Refused> {
        self.ask(Access::write(space, address, width, value)).0.map(drop)
    }

    fn memory(&self, address: u64, width: Width) -> u64 {
        self.read(Space::SystemMemory, address, width).unwrap_or_else(|why| panic!("acpi: a read of the firmware's own memory was refused: {why:?}"))
    }

    /// The first table the XSDT lists under `signature`.
    fn table(&self, rsdp: u64, signature: &[u8; 4]) -> u64 {
        // ACPI 6.5 Table 5.3: `XsdtAddress` at 24; Table 5.4: a table's length at 4, and the XSDT's entries from 36.
        let xsdt = self.memory(rsdp + 24, Width::QWord);
        let entries = (self.memory(xsdt + 4, Width::DWord) - 36) / 8;
        (0..entries)
            .map(|i| self.memory(xsdt + 36 + i * 8, Width::QWord))
            .find(|&table| self.memory(table, Width::DWord) == u64::from(u32::from_le_bytes(*signature)))
            .unwrap_or_else(|| panic!("acpi: the XSDT lists no {:?}", String::from_utf8_lossy(signature)))
    }
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        None if std::fs::exists(OUTLIVED).expect("acpi: ask ROOT for this boot's arm") => outlived(),
        None => probe(),
        Some("keeper") => keeper(),
        Some("supplier") => supplier(),
        other => panic!("acpi: unknown role {other:?}"),
    }
}

fn claim(cap: &SysCap) -> Device {
    let by = Instant::now() + RELEASED;
    loop {
        match cap.claim(DeviceType::Acpi) {
            Ok(claim) => return claim,
            Err(SyscallError::AlreadyExists) => {}
            Err(other) => panic!("acpi: the fixed hardware's claim answered {other:?}"),
        }
        assert!(Instant::now() < by, "acpi: the claim never came back in {RELEASED:?}");
        std::thread::yield_now();
    }
}

/// Read the claim's description, which binds it to this process.
fn bind(claim: &Device) -> AcpiInfo {
    let mut bytes = [0u8; size_of::<AcpiInfo>()];
    let n = claim.read(&mut bytes).expect("acpi: the claim's first read is its description");
    assert_eq!(n, bytes.len(), "acpi: a description of {n} bytes");
    // SAFETY: `AcpiInfo` is `repr(C)` integers with no padding, so every bit pattern of its size is one.
    unsafe { core::ptr::read_unaligned(bytes.as_ptr().cast()) }
}

fn probe() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a device-minting capability");

    // No server ran on this boot, and nothing has held the claim.
    assert_eq!(
        toyos::power::stop(toyos::power::Stop::Shutdown),
        toyos::power::Refused::Kernel(SyscallError::NotSupported),
        "acpi: a power-off with no sleep type supplied"
    );

    // A holder that dies with the lock: the kernel gives it back with the claim.
    let kept = Command::new(SELF_PATH)
        .arg("keeper")
        .endow(CLAIM_LABEL, claim(&cap).into_raw().0)
        .stdout(Stdio::piped())
        .output()
        .expect("acpi: spawn the keeper");
    let said = String::from_utf8_lossy(&kept.stdout);
    assert!(kept.status.success() && said.contains(KEPT), "acpi: the keeper ended {:?} having said {said:?}", kept.status);

    let holder = Holder(claim(&cap));

    // Unbound, the claim answers nothing: its first read is what makes this
    // process its holder.
    let mut post = Access::write(Space::SystemIo, 0x80, Width::Byte, 0);
    assert_eq!(syscall::acpi_access(holder.handle(), &mut post), Err(SyscallError::PermissionDenied));
    assert_eq!(syscall::acpi_lock_take(holder.handle()), Err(SyscallError::PermissionDenied));
    assert_eq!(syscall::acpi_s5(holder.handle(), Q35_SLP_TYP_A), Err(SyscallError::PermissionDenied));
    let info = bind(&holder.0);
    println!("acpi: an unbound claim was refused its access, the lock and the power-off's sleep type");

    // A request that names no space, width or direction, a value wider than
    // its width, and a reserved byte that is set.
    let io = Access::read(Space::SystemIo, 0x80, Width::Byte);
    for malformed in [
        Access { space: 3, ..io },
        Access { width: 3, ..io },
        Access { width: 0, ..io },
        Access { write: 2, ..io },
        Access { write: 1, value: 0x100, ..io },
        Access { reserved: [0, 0, 1], ..io },
    ] {
        let mut asked = malformed;
        assert_eq!(syscall::acpi_access(holder.handle(), &mut asked), Err(SyscallError::InvalidArgument), "{malformed:?}");
        assert_eq!(asked, malformed, "acpi: a refused request was written to");
    }

    memory(&holder, &info);
    ports(&holder, &info);
    firmware(&holder, &info, &cap);
    configuration(&holder, &info);
    lock(&holder, &info);
    sleep_type(&holder);

    assert_eq!(syscall::acpi_lock_take(holder.handle()), Ok(true), "acpi: the lock, for the power-off to find");
    println!("{HELD_INTO_THE_STOP}");
    let refused = toyos::power::stop(toyos::power::Stop::Shutdown);
    panic!("acpi: the power-off was refused: {refused:?}");
}

/// What the probe says once every arm above has passed, holding the lock.
const HELD_INTO_THE_STOP: &str = "acpi: holding the Global Lock, and asking for the power-off with it";

fn memory(holder: &Holder, info: &AcpiInfo) {
    // RAM: the megabyte's first page, which a guest's firmware hands over as memory.
    for width in [Width::Byte, Width::QWord] {
        let (read, ty) = holder.ask(Access::read(Space::SystemMemory, 0x10_0000, width));
        assert_eq!(read, Err(Refused::UsableMemory), "acpi: RAM was read");
        assert!(USABLE.contains(&ty), "acpi: RAM answered memory type {ty}");
        assert_eq!(holder.write(Space::SystemMemory, 0x10_0000, width, 0), Err(Refused::UsableMemory), "acpi: RAM was written");
    }
    // This program's own stack is RAM somewhere, and what it holds is still its own.
    let canary = std::hint::black_box(0x5AFE_C0DE_5AFE_C0DEu64);
    assert_eq!(canary, 0x5AFE_C0DE_5AFE_C0DE);
    println!("acpi: RAM was refused both ways as UsableMemory");

    // The tables: read, with the bytes firmware put there, and never written.
    let (signature, ty) = holder.ask(Access::read(Space::SystemMemory, info.rsdp, Width::QWord));
    assert_eq!(signature, Ok(u64::from_le_bytes(*b"RSD PTR ")), "acpi: the RSDP's signature");
    assert_eq!(ty, ACPI_RECLAIM, "acpi: this firmware keeps its RSDP in memory type {ty}");
    assert_eq!(holder.write(Space::SystemMemory, info.rsdp, Width::Byte, b'X'.into()), Err(Refused::TableWrite));
    assert_eq!(holder.memory(info.rsdp, Width::Byte), u64::from(b'R'), "acpi: a refused write reached the RSDP");
    // An unaligned word and dword of it are one access each.
    assert_eq!(holder.memory(info.rsdp + 1, Width::Word), u64::from(u16::from_le_bytes(*b"SD")));
    assert_eq!(holder.memory(info.rsdp + 1, Width::DWord), u64::from(u32::from_le_bytes(*b"SD P")));
    println!("acpi: the RSDP read through as type {ty} and its write was refused TableWrite");

    // An address firmware's map does not list, between the PCI hole's start
    // and the ECAM window, where this guest's firmware has the boot processor's range registers
    // type everything from the top of low RAM to 4 GiB uncacheable: a
    // register's address, read and never written. And one the map does not
    // list below 1 MiB, the legacy video hole, which the kernel calls no
    // register: the fixed range registers decide there, and it reads none.
    let (hole, ty) = holder.ask(Access::read(Space::SystemMemory, 0xD000_0000, Width::DWord));
    assert!(hole.is_ok() && ty == UNLISTED, "acpi: an unlisted address the range registers type uncacheable answered {hole:?}, type {ty}");
    let (hole, ty) = holder.ask(Access::write(Space::SystemMemory, 0xD000_0000, Width::DWord, 0));
    assert_eq!((hole, ty), (Err(Refused::MemoryType), UNLISTED), "acpi: an unlisted address was written");
    let (hole, ty) = holder.ask(Access::read(Space::SystemMemory, 0xA_0000, Width::DWord));
    assert_eq!((hole, ty), (Err(Refused::UnlistedCached), UNLISTED), "acpi: an unlisted address below 1 MiB");
    // The local APIC; the I/O APIC, which the kernel drives through its
    // first 0x20 bytes, at its first register, at the EOI register a chipset
    // keeps at 0x40 and at its page's last dword; and the HPET. Each is a
    // device's by the kernel's own record of it: this firmware's map lists
    // none of them, and an address it does not list answers `MemoryType`.
    for device in [0xFEE0_0000u64, 0xFEC0_0000, 0xFEC0_0040, 0xFEC0_0FFC, 0xFED0_0000] {
        assert_eq!(holder.read(Space::SystemMemory, device, Width::DWord), Err(Refused::DeviceMemory), "acpi: {device:#x} was read");
        assert_eq!(holder.write(Space::SystemMemory, device, Width::DWord, 0), Err(Refused::DeviceMemory), "acpi: {device:#x} was written");
    }
    // A memory BAR of a function no kernel driver maps: the network card,
    // which is a claim's and which nothing in this boot claims, so its BAR
    // stays where firmware put it.
    let bar = nic_bar(holder);
    assert_eq!(holder.read(Space::SystemMemory, bar, Width::DWord), Err(Refused::DeviceMemory), "acpi: a function's BAR at {bar:#x} was read");
    assert_eq!(holder.write(Space::SystemMemory, bar + 0x14, Width::DWord, 0), Err(Refused::DeviceMemory), "acpi: a function's BAR was written");
    assert_eq!(holder.read(Space::SystemMemory, u64::MAX, Width::Word), Err(Refused::Unmapped));
    println!("acpi: an unlisted register was read and refused its write MemoryType, an unlisted address below 1 MiB was refused UnlistedCached, and the interrupt controllers, the HPET and a function's BAR DeviceMemory");

    // Non-volatile memory, both ways: the FACS, and the bytes after it.
    let fadt = holder.table(info.rsdp, b"FACP");
    // Table 5.9: `FIRMWARE_CTRL` at 36, `X_FIRMWARE_CTRL` at 132.
    let facs = match holder.memory(fadt + 132, Width::QWord) {
        0 => holder.memory(fadt + 36, Width::DWord),
        wide => wide,
    };
    let (signature, ty) = holder.ask(Access::read(Space::SystemMemory, facs, Width::DWord));
    assert_eq!(signature, Ok(u64::from(u32::from_le_bytes(*b"FACS"))));
    assert_eq!(ty, ACPI_NVS, "acpi: this firmware keeps its FACS in memory type {ty}");
    let len = holder.memory(facs + 4, Width::DWord);
    for (at, width) in [(facs + 16, Width::DWord), (facs, Width::Byte), (facs + len - 1, Width::Byte), (facs - 4, Width::QWord)] {
        assert_eq!(holder.write(Space::SystemMemory, at, width, 0), Err(Refused::FacsWrite), "acpi: a write at {at:#x}");
    }
    let beside = facs + len;
    let was = holder.memory(beside, Width::QWord);
    assert_eq!(holder.write(Space::SystemMemory, beside, Width::QWord, !was), Ok(()), "acpi: a write to non-volatile memory");
    assert_eq!(holder.memory(beside, Width::QWord), !was, "acpi: the write did not land");
    assert_eq!(holder.write(Space::SystemMemory, beside, Width::QWord, was), Ok(()));
    println!("acpi: the FACS read through as type {ty}, its write was refused FacsWrite, and the memory after it was written and put back");
}

/// Where the first memory BAR of the guest's network card is, read from its
/// configuration space as a holder reads it (PCI Local Bus 3.0 §6.2.5.1: bit
/// 0 clear is memory, and bits 2:1 of `10b` a 64-bit address whose high half
/// is the next register).
fn nic_bar(holder: &Holder) -> u64 {
    // virtio-net as a modern device (virtio 1.2 §4.1.2: device id 0x1040 + 1).
    const NIC: u64 = 0x1041_1af4;
    let config = |device: u8, offset: u16| holder.read(Space::PciConfig, pci_address(0, 0, device, 0, offset), Width::DWord).expect("acpi: a configuration read on bus 0");
    let device = (0..32).find(|&device| config(device, 0) == NIC).expect("acpi: no virtio network card on this guest's bus 0");
    (0..6)
        .map(|slot| (config(device, 0x10 + slot * 4), slot))
        .find(|&(low, _)| low != 0 && low & 1 == 0)
        .map(|(low, slot)| {
            let high = if low >> 1 & 3 == 2 { config(device, 0x14 + slot * 4) } else { 0 };
            high << 32 | low & !0xF
        })
        .expect("acpi: the network card has no memory BAR")
}

fn ports(holder: &Holder, info: &AcpiInfo) {
    // COM1 and the CMOS index: the kernel's, both ways.
    for port in [0x3F8u64, 0x3FD, 0x70, 0x20, 0xCF8] {
        assert_eq!(holder.read(Space::SystemIo, port, Width::Byte), Err(Refused::KernelPort), "acpi: port {port:#x} was read");
        assert_eq!(holder.write(Space::SystemIo, port, Width::Byte, b'!'.into()), Err(Refused::KernelPort), "acpi: port {port:#x} was written");
    }
    assert_eq!(holder.write(Space::SystemIo, 0x3F7, Width::Word, 0), Err(Refused::KernelPort), "acpi: a word that ends on COM1");
    assert_eq!(holder.read(Space::SystemIo, 0xFFFF, Width::Word), Err(Refused::PortSpan));
    assert_eq!(holder.read(Space::SystemIo, 0x1_0000, Width::Byte), Err(Refused::PortSpan));
    assert_eq!(holder.read(Space::SystemIo, 0x80, Width::QWord), Err(Refused::PortSpan));

    // The i8042's row, which another claim is for: this boot's kernel leaves
    // the controller unprobed, so nothing declared its ports.
    for port in [0x60u64, 0x64] {
        assert_eq!(holder.read(Space::SystemIo, port, Width::Byte), Err(Refused::ClaimedPort), "acpi: port {port:#x} was read");
        assert_eq!(holder.write(Space::SystemIo, port, Width::Byte, 0), Err(Refused::ClaimedPort), "acpi: port {port:#x} was written");
    }
    assert_eq!(holder.read(Space::SystemIo, 0x5F, Width::Word), Err(Refused::ClaimedPort), "acpi: a word that ends on the i8042's data port");

    // The POST port, which the kernel declared and opens.
    assert_eq!(holder.write(Space::SystemIo, 0x80, Width::Byte, 0x5A), Ok(()), "acpi: the POST port");

    // `PM1a_CNT`, as the FADT names it (Table 5.9, at 64): read, never written.
    let fadt = holder.table(info.rsdp, b"FACP");
    let control = holder.memory(fadt + 64, Width::DWord);
    let held = holder.read(Space::SystemIo, control, Width::Word).expect("acpi: PM1a_CNT reads");
    assert_eq!(held & 1, 1, "acpi: PM1a_CNT reads {held:#06x}, SCI_EN clear, on a machine a claim put in ACPI mode");
    assert_eq!(holder.write(Space::SystemIo, control, Width::Word, held), Err(Refused::ReadOnlyPort), "acpi: PM1a_CNT was written");

    // The claim's own event block, which nothing declared, and a dword of it.
    let status = u64::from(info.pm1_event.port);
    holder.read(Space::SystemIo, status, Width::Word).expect("acpi: the claim's own PM1 status");
    holder.read(Space::SystemIo, status, Width::DWord).expect("acpi: the claim's own PM1 event block as a dword");
    println!("acpi: COM1, the CMOS index, the 8259 and the configuration mechanism were refused KernelPort; the i8042's row ClaimedPort; PM1a_CNT read and refused its write ReadOnlyPort; the POST port was written");
}

/// Bytes this guest's FADT gives no meaning, which its chipset model keeps
/// and does nothing on: the calls asked from a thread off the boot processor
/// where there is one, and the two the storm alternates.
const CROSSED: [u64; 3] = [0x51, 0x52, 0x53];
const STORMED: [u64; 2] = [0x54, 0x55];

/// Calls the storm asks at most: what firmware's AML asks of a handler that
/// never answers, one a millisecond for ten seconds.
const STORM: usize = 10_000;

/// Threads started in turn before one must have found itself off the boot
/// processor, as `acpi_release`'s.
const STARTS: usize = 64;

/// What the boot processor counts of the kernel's writes to `SMI_CMD`, read
/// in a round of the counters issued after the last one read here returned: a
/// read within the kernel's joining window of a round answers with that
/// round, which may be older than what the caller just did, so this reads
/// until the boot processor's stamp is one it has not seen. Nothing else on
/// this boot reads the counters, so a round not seen is one this asked for.
struct Counted<'a> {
    cap: &'a SysCap,
    seen: Option<u64>,
}

impl Counted<'_> {
    fn firmware_calls(&mut self) -> u64 {
        loop {
            let mut raw = vec![RawRecord::EMPTY; syscall::cpu_count() as usize];
            let n = self.cap.counters(&mut raw).expect("acpi: the estate's capability reads the counters");
            let records: Vec<Record> = raw[..n].iter().map(|r| Record::decode(r).expect("acpi: a record that decodes")).collect();
            let stamp = records[0].get(Counter::Stamp);
            if records.iter().any(|r| r.stale) || stamp == self.seen {
                std::thread::yield_now();
                continue;
            }
            self.seen = stamp;
            // No other CPU holds the counter: none writes the port.
            for r in &records[1..] {
                assert_eq!(r.get(Counter::FirmwareCalls), None, "acpi: cpu{} counts firmware calls, and makes none", r.cpu);
            }
            return records[0].get(Counter::FirmwareCalls).expect("acpi: the boot processor counts its writes to SMI_CMD");
        }
    }
}

/// One byte for `SMI_CMD`, asked by a thread that read itself off the boot
/// processor as its first act where the machine has another CPU: the x2APIC
/// id it read, 0 on a machine of one.
fn call_off_the_boot_processor(holder: &Holder, smi_cmd: u64, value: u64) -> u32 {
    let claim = holder.handle();
    let ask = || {
        let mut access = Access::write(Space::SystemIo, smi_cmd, Width::Byte, value);
        assert_eq!(syscall::acpi_access(claim, &mut access), Ok(Ok(value)), "acpi: firmware call {value:#04x}");
    };
    if syscall::cpu_count() == 1 {
        ask();
        return 0;
    }
    for _ in 0..STARTS {
        let on = std::thread::scope(|threads| {
            let asker = threads.spawn(|| {
                let on = cpu::x2apic_id();
                (on != 0).then(|| {
                    ask();
                    on
                })
            });
            asker.join().expect("acpi: the calling thread")
        });
        if let Some(on) = on {
            return on;
        }
    }
    panic!("acpi: {STARTS} threads in a row started on the boot processor");
}

fn firmware(holder: &Holder, info: &AcpiInfo, cap: &SysCap) {
    // Table 5.9: `SMI_CMD` at 48, `ACPI_ENABLE` at 52, `ACPI_DISABLE` at 53, `PM1a_CNT_BLK` at 64.
    let fadt = holder.table(info.rsdp, b"FACP");
    let smi_cmd = holder.memory(fadt + 48, Width::DWord);
    assert_ne!(smi_cmd, 0, "acpi: this firmware names no SMI_CMD");
    let named = [holder.memory(fadt + 52, Width::Byte), holder.memory(fadt + 53, Width::Byte)];
    let control = holder.memory(fadt + 64, Width::DWord);
    let sci_en = || holder.read(Space::SystemIo, control, Width::Word).expect("acpi: PM1a_CNT reads") & 1;
    let last_written = || holder.read(Space::SystemIo, smi_cmd, Width::Byte).expect("acpi: SMI_CMD reads");
    let write = |at: u64, width: Width, value: u64| holder.write(Space::SystemIo, at, width, value);
    let mut counted = Counted { cap, seen: None };
    let hex = |bytes: &[u64]| bytes.iter().map(|byte| format!("{byte:#04x}")).collect::<Vec<_>>().join(", ");
    // Whether this chipset interrupts its firmware on a write to the port:
    // the ICH9's `SMI_EN` at PMBASE + 30h, `APMC_EN` its bit 5, PMBASE being
    // where the FADT puts the PM1a event block. A reading.
    let smi_en = holder.read(Space::SystemIo, u64::from(info.pm1_event.port) + 0x30, Width::DWord).expect("acpi: SMI_EN reads");

    // The kernel's own commands: refused, and the chipset saw neither, which
    // leaves ACPI mode on the second.
    let before = (counted.firmware_calls(), last_written());
    assert!(named.iter().all(|&byte| byte != 0 && !CROSSED.contains(&byte) && !STORMED.contains(&byte)), "acpi: this FADT names {}", hex(&named));
    for kept in named {
        assert_eq!(write(smi_cmd, Width::Byte, kept), Err(Refused::KernelCommand), "acpi: {kept:#04x}, which the FADT names, was written to SMI_CMD");
    }
    assert_eq!(sci_en(), 1, "acpi: SCI_EN reads clear after a refused ACPI_DISABLE");
    // A command is one byte to the one port.
    for (at, width) in [(smi_cmd, Width::Word), (smi_cmd, Width::DWord), (smi_cmd - 1, Width::Word), (smi_cmd - 3, Width::DWord)] {
        assert_eq!(write(at, width, CROSSED[0]), Err(Refused::CommandSpan), "acpi: a {width:?} at {at:#x}");
    }
    assert_eq!((counted.firmware_calls(), last_written()), before, "acpi: a refused command was written or counted");

    // A call: the chipset has the byte, and the boot processor counted one write.
    let mut asked_from = vec![call_off_the_boot_processor(holder, smi_cmd, CROSSED[0])];
    assert_eq!(last_written(), CROSSED[0], "acpi: SMI_CMD does not hold the byte the kernel was asked to write");
    assert_eq!(counted.firmware_calls(), before.0 + 1, "acpi: one firmware call");
    for value in &CROSSED[1..] {
        asked_from.push(call_off_the_boot_processor(holder, smi_cmd, *value));
        assert_eq!(last_written(), *value);
    }
    assert_eq!(sci_en(), 1, "acpi: SCI_EN reads clear after calls of {}", hex(&CROSSED));

    // The storm: the same call asked over and over is made a few times and
    // then refused, and the refused byte never reached the port.
    let mut made = CROSSED.len();
    let stormed = (0..STORM).find(|i| {
        let value = STORMED[i % 2];
        match write(smi_cmd, Width::Byte, value) {
            Ok(()) => {
                made += 1;
                assert_eq!(last_written(), value);
                false
            }
            Err(Refused::CommandRate) => true,
            Err(other) => panic!("acpi: firmware call {value:#04x} was refused {other:?}"),
        }
    });
    let refused_at = stormed.unwrap_or_else(|| panic!("acpi: {STORM} firmware calls in a row were made, and none refused"));
    assert_ne!(last_written(), STORMED[refused_at % 2], "acpi: a call refused CommandRate reached SMI_CMD");
    assert_eq!(counted.firmware_calls(), before.0 + made as u64, "acpi: the boot processor counted another number of writes than were made");
    println!(
        "acpi: firmware calls of {} were asked from the CPUs of x2APIC id {asked_from:?}; this chipset's SMI_EN reads {smi_en:#010x}, APMC_EN {}",
        hex(&CROSSED),
        if smi_en & 1 << 5 == 0 { "clear, so no call interrupts its firmware" } else { "set, so each call interrupts its firmware" },
    );
    println!("acpi: {made} firmware calls were made before call {} of the storm was refused", refused_at + 1);
    println!("acpi: ACPI_ENABLE and ACPI_DISABLE were refused SMI_CMD as KernelCommand and SCI_EN stayed set, a write wider than a byte was refused CommandSpan, every firmware call made was read back from the port and counted on the boot processor, and a storm of them was refused CommandRate");
}

fn configuration(holder: &Holder, info: &AcpiInfo) {
    let host_bridge = |offset| pci_address(0, 0, 0, 0, offset);
    let id = holder.read(Space::PciConfig, host_bridge(0), Width::DWord).expect("acpi: the host bridge's identity");
    assert!(id as u16 != 0xFFFF, "acpi: no function answers at 00:00.0 ({id:#010x})");
    assert_eq!(holder.read(Space::PciConfig, host_bridge(0), Width::Word), Ok(id & 0xFFFF));
    assert_eq!(holder.read(Space::PciConfig, host_bridge(2), Width::Word), Ok(id >> 16));
    assert_eq!(holder.read(Space::PciConfig, host_bridge(3), Width::Byte), Ok(id >> 24));
    // No function answers on the last bus's last device: all ones, not a refusal.
    assert_eq!(holder.read(Space::PciConfig, pci_address(0, 0xFF, 31, 7, 0), Width::DWord), Ok(0xFFFF_FFFF));
    assert_eq!(holder.read(Space::PciConfig, host_bridge(0), Width::QWord), Err(Refused::ConfigSpan));
    assert_eq!(holder.read(Space::PciConfig, host_bridge(2), Width::DWord), Err(Refused::ConfigSpan));
    assert_eq!(holder.read(Space::PciConfig, host_bridge(0x1000), Width::Byte), Err(Refused::ConfigSpan));
    assert_eq!(holder.read(Space::PciConfig, pci_address(1, 0, 0, 0, 0), Width::DWord), Err(Refused::ConfigUnreachable));
    assert_eq!(holder.read(Space::PciConfig, 1 << 48, Width::DWord), Err(Refused::ConfigUnreachable));

    // The same register through the ECAM window is the same access: the
    // MCFG's first allocation names the window's base at 44.
    let ecam = holder.memory(holder.table(info.rsdp, b"MCFG") + 44, Width::QWord);
    assert_eq!(holder.read(Space::SystemMemory, ecam, Width::DWord), Ok(id), "acpi: the host bridge through ECAM");
    assert_eq!(holder.read(Space::SystemMemory, ecam, Width::QWord), Err(Refused::ConfigSpan));

    // No write, by a function's address or through the ECAM window: the
    // header, a register past it with the value it holds, extended space, a
    // function nothing answers at, and a shape no read has.
    let held = holder.read(Space::PciConfig, host_bridge(0x44), Width::Byte).expect("acpi: a register past the header");
    for (offset, width, value) in [(0x04, Width::Word, 0), (0x10, Width::DWord, 0), (0x3C, Width::Byte, 0), (0x44, Width::Byte, held), (0x44, Width::Byte, !held & 0xFF), (0x100, Width::DWord, 0)] {
        assert_eq!(holder.write(Space::PciConfig, host_bridge(offset), width, value), Err(Refused::ConfigWrite), "acpi: {offset:#x} by address");
        assert_eq!(holder.write(Space::SystemMemory, ecam + u64::from(offset), width, value), Err(Refused::ConfigWrite), "acpi: {offset:#x} through ECAM");
    }
    assert_eq!(holder.read(Space::PciConfig, host_bridge(0x44), Width::Byte), Ok(held), "acpi: a refused write reached the host bridge");
    assert_eq!(holder.write(Space::PciConfig, pci_address(0, 0xFF, 31, 7, 0x44), Width::Byte, 0), Err(Refused::ConfigWrite));
    assert_eq!(holder.write(Space::PciConfig, host_bridge(2), Width::DWord, 0), Err(Refused::ConfigWrite));
    println!("acpi: the host bridge read as {id:#010x} by its address and through ECAM, and every write to configuration space was refused ConfigWrite");
}

fn lock(holder: &Holder, info: &AcpiInfo) {
    let fadt = holder.table(info.rsdp, b"FACP");
    let facs = match holder.memory(fadt + 132, Width::QWord) {
        0 => holder.memory(fadt + 36, Width::DWord),
        wide => wide,
    };
    let word = |holder: &Holder| holder.memory(facs + 16, Width::DWord);
    const PENDING: u64 = 1;
    const OWNED: u64 = 2;

    assert_eq!(word(holder), 0, "acpi: the keeper's lock outlived its claim");
    assert_eq!(syscall::acpi_lock_release(holder.handle()), Err(SyscallError::InvalidArgument), "acpi: a lock nobody took was given back");
    assert_eq!(syscall::acpi_lock_take(holder.handle()), Ok(true));
    assert_eq!(word(holder), OWNED);
    assert_eq!(syscall::acpi_lock_take(holder.handle()), Err(SyscallError::AlreadyExists), "acpi: a second take");
    assert_eq!(syscall::acpi_lock_release(holder.handle()), Ok(()));
    assert_eq!(word(holder), 0);

    // The firmware asks while the holder has it: the release clears the
    // word and tells the firmware by `GBL_RLS`, bit 2 of `PM1a_CNT` (ACPI 6.5
    // §4.8.3.2), which this guest's model keeps as written where a chipset
    // reads it back clear.
    const GBL_RLS: u64 = 1 << 2;
    // Table 5.9: `PM1a_CNT_BLK` at 64.
    let control = holder.memory(fadt + 64, Width::DWord);
    let told = |holder: &Holder| holder.read(Space::SystemIo, control, Width::Word).expect("acpi: PM1a_CNT reads") & GBL_RLS;
    assert_eq!(syscall::acpi_lock_take(holder.handle()), Ok(true));
    assert_eq!(syscall::debug_with(debug_action::ACPI_FIRMWARE_LOCK, debug_action::FIRMWARE_ASKS), OWNED);
    assert_eq!(word(holder), OWNED | PENDING);
    assert_eq!(told(holder), 0, "acpi: GBL_RLS reads set before any release owed it");
    assert_eq!(syscall::acpi_lock_release(holder.handle()), Ok(()));
    assert_eq!(word(holder), 0);
    assert_eq!(told(holder), GBL_RLS, "acpi: a release the firmware had asked for wrote no GBL_RLS");

    // The firmware owns it: the take is refused and leaves its request.
    assert_eq!(syscall::debug_with(debug_action::ACPI_FIRMWARE_LOCK, debug_action::FIRMWARE_OWNS), 0);
    assert_eq!(syscall::acpi_lock_take(holder.handle()), Ok(false), "acpi: a lock the firmware owns was taken");
    assert_eq!(word(holder), OWNED | PENDING);
    assert_eq!(syscall::acpi_lock_take(holder.handle()), Ok(false));
    assert_eq!(syscall::acpi_lock_release(holder.handle()), Err(SyscallError::InvalidArgument), "acpi: the firmware's lock was given back");
    assert_eq!(word(holder), OWNED | PENDING, "acpi: a refused release changed the word");
    // The firmware lets go, and the next take has it; the stale request goes with the take.
    assert_eq!(syscall::debug_with(debug_action::ACPI_FIRMWARE_LOCK, debug_action::FIRMWARE_FREES), OWNED | PENDING);
    assert_eq!(syscall::acpi_lock_take(holder.handle()), Ok(true));
    assert_eq!(word(holder), OWNED);
    assert_eq!(syscall::acpi_lock_release(holder.handle()), Ok(()));
    println!("acpi: the lock a dead holder left taken read free; it was taken and given back, given back with GBL_RLS where the firmware had asked, and found pending while the firmware owned it");
}

/// What this guest's `\_S5` names for `SLP_TYPa` (ACPI 6.5 §7.4.2), as
/// `acpiserver` evaluates it on the boots that start one: QEMU's ICH9 powers
/// off on it.
const Q35_SLP_TYP_A: u64 = 0;

/// A sleep type the register holds and QEMU's ICH9 does nothing on: a
/// power-off that entered it would still be running.
const NO_SLEEP_OF_Q35S: u64 = 5;

fn sleep_type(holder: &Holder) {
    // `SLP_TYPx` is three bits (Table 4.16): bit 3 would land on `SLP_EN`.
    for wide in [8u64, 0x100, 1 << 32 | 5, u64::MAX] {
        assert_eq!(syscall::acpi_s5(holder.handle(), wide), Err(SyscallError::InvalidArgument), "acpi: {wide:#x} as a sleep type");
    }
    // The keeper's turn went with its claim, so this holder supplies its own.
    assert_eq!(syscall::acpi_s5(holder.handle(), Q35_SLP_TYP_A), Ok(()));
    for second in [NO_SLEEP_OF_Q35S, Q35_SLP_TYP_A] {
        assert_eq!(syscall::acpi_s5(holder.handle(), second), Err(SyscallError::AlreadyExists), "acpi: a second sleep type under one claim");
    }
    println!("acpi: a sleep type wider than three bits was refused InvalidArgument, the next holder's replaced a dead one's, and a second under one claim was refused AlreadyExists");
}

/// What the keeper says once it holds the lock.
const KEPT: &str = "keeper: holding the Global Lock, and leaving with it";

fn keeper() {
    let claim: Device = Endowments::get().take(CLAIM_LABEL).expect("acpi: the keeper is endowed the claim");
    bind(&claim);
    assert_eq!(syscall::acpi_s5(claim.as_handle(), NO_SLEEP_OF_Q35S), Ok(()), "keeper: a sleep type");
    assert_eq!(syscall::acpi_s5(claim.as_handle(), Q35_SLP_TYP_A), Err(SyscallError::AlreadyExists), "keeper: a second sleep type");
    assert_eq!(syscall::acpi_lock_take(claim.as_handle()), Ok(true), "keeper: the lock");
    println!("{KEPT}");
}

/// What the supplier says once the kernel has its sleep type.
const SUPPLIED: &str = "supplier: this guest's own sleep type supplied, and leaving";

fn supplier() {
    let claim: Device = Endowments::get().take(CLAIM_LABEL).expect("acpi: the supplier is endowed the claim");
    bind(&claim);
    assert_eq!(syscall::acpi_s5(claim.as_handle(), Q35_SLP_TYP_A), Ok(()), "supplier: this guest's sleep type");
    println!("{SUPPLIED}");
}

/// What [`outlived`] says before it asks, holding no claim.
const ASKED_WITH_NO_HOLDER: &str = "acpi: asking for the power-off with no claim held, on what a holder that is gone supplied";

fn outlived() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a device-minting capability");
    let left = Command::new(SELF_PATH)
        .arg("supplier")
        .endow(CLAIM_LABEL, claim(&cap).into_raw().0)
        .stdout(Stdio::piped())
        .output()
        .expect("acpi: spawn the supplier");
    let said = String::from_utf8_lossy(&left.stdout);
    assert!(left.status.success() && said.contains(SUPPLIED), "acpi: the supplier ended {:?} having said {said:?}", left.status);
    // The row is claimable again only once the supplier's claim was
    // released; and this claim's own release, which supplied nothing, has
    // returned before the power-off is asked for.
    drop(claim(&cap));
    println!("{ASKED_WITH_NO_HOLDER}");
    let refused = toyos::power::stop(toyos::power::Stop::Shutdown);
    panic!("acpi: the power-off was refused: {refused:?}");
}
