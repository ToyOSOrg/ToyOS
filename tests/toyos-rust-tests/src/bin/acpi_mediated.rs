//! The `acpi` claim's mediated access, asked as its holder asks: what the
//! kernel reads and writes for it, what it refuses and by which name, and the
//! firmware's Global Lock taken, found owned and given back.
//!
//! Run on a boot that starts no ACPI server (`tests/acpicase`), on a guest:
//! every write here that must be refused is one a kernel that made it would
//! make for real — to RAM, to the tables, to COM1, to `PM1a_CNT` — and the
//! lock's owned state is staged on the FACS itself, which only a machine
//! whose firmware is not using it can take. Each address is found as a holder
//! finds it, from the RSDP the claim's description names.
//!
//! First a child is handed the claim, takes the lock and exits with it: the
//! claim binds to one process for that process's life, so the parent claims
//! only after, and reads the lock word free.

use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos::{AsHandle, Device};
use toyos_abi::acpi::{pci_address, Access, AcpiInfo, Refused, Space, Width, UNLISTED};
use toyos_abi::syscall::{self, debug_action, DeviceType, SyscallError};
use toyos_abi::RawHandle;

const SELF_PATH: &str = "/system/bin/test_rs_acpi_mediated";
const CLAIM_LABEL: &str = "acpi-claim";

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
        None => probe(),
        Some("keeper") => keeper(),
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
    let info = bind(&holder.0);
    println!("acpi: an unbound claim was refused its access and the lock");

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
    configuration(&holder, &info);
    lock(&holder, &info);
}

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

    // An address firmware's map does not list, between the PCI hole's start and the ECAM window.
    let (hole, ty) = holder.ask(Access::read(Space::SystemMemory, 0xD000_0000, Width::DWord));
    assert_eq!((hole, ty), (Err(Refused::MemoryType), UNLISTED), "acpi: an unlisted address");
    // The local APIC, the I/O APIC and the HPET, wherever the map puts them.
    for device in [0xFEE0_0000u64, 0xFEC0_0000, 0xFED0_0000] {
        let refused = holder.read(Space::SystemMemory, device, Width::DWord);
        assert!(matches!(refused, Err(Refused::KernelDevice | Refused::MemoryType)), "acpi: {device:#x} answered {refused:?}");
        assert_eq!(holder.write(Space::SystemMemory, device, Width::DWord, 0), refused.map(drop));
    }
    assert_eq!(holder.read(Space::SystemMemory, u64::MAX, Width::Word), Err(Refused::Unmapped));
    println!("acpi: an unlisted address, the interrupt controllers and the HPET were refused");

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

    // The POST port, which the kernel declared and opens.
    assert_eq!(holder.write(Space::SystemIo, 0x80, Width::Byte, 0x5A), Ok(()), "acpi: the POST port");

    // `PM1a_CNT` and `SMI_CMD`, as the FADT names them (Table 5.9, at 64 and 48): read, never written.
    let fadt = holder.table(info.rsdp, b"FACP");
    let control = holder.memory(fadt + 64, Width::DWord);
    let held = holder.read(Space::SystemIo, control, Width::Word).expect("acpi: PM1a_CNT reads");
    assert_eq!(held & 1, 1, "acpi: PM1a_CNT reads {held:#06x}, SCI_EN clear, on a machine a claim put in ACPI mode");
    assert_eq!(holder.write(Space::SystemIo, control, Width::Word, held), Err(Refused::ReadOnlyPort), "acpi: PM1a_CNT was written");
    let smi_cmd = holder.memory(fadt + 48, Width::DWord);
    assert_ne!(smi_cmd, 0, "acpi: this firmware names no SMI_CMD");
    holder.read(Space::SystemIo, smi_cmd, Width::Byte).expect("acpi: SMI_CMD reads");
    assert_eq!(holder.write(Space::SystemIo, smi_cmd, Width::Byte, 0), Err(Refused::ReadOnlyPort), "acpi: SMI_CMD was written");

    // The claim's own event block, which nothing declared, and a dword of it.
    let status = u64::from(info.pm1_event.port);
    holder.read(Space::SystemIo, status, Width::Word).expect("acpi: the claim's own PM1 status");
    holder.read(Space::SystemIo, status, Width::DWord).expect("acpi: the claim's own PM1 event block as a dword");
    println!("acpi: COM1, the CMOS index, the 8259 and the configuration mechanism were refused KernelPort; PM1a_CNT and SMI_CMD read and refused their write ReadOnlyPort; the POST port was written");
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
    assert_eq!(holder.write(Space::SystemMemory, ecam + 4, Width::Word, 0), Err(Refused::ConfigHeader), "acpi: the command register through ECAM");

    // Writes: the header and extended space never, a register past both on a
    // function nothing drives with the value it holds.
    for (offset, width) in [(0x04, Width::Word), (0x10, Width::DWord), (0x3C, Width::Byte)] {
        assert_eq!(holder.write(Space::PciConfig, host_bridge(offset), width, 0), Err(Refused::ConfigHeader), "acpi: {offset:#x}");
    }
    assert_eq!(holder.write(Space::PciConfig, host_bridge(0x100), Width::DWord, 0), Err(Refused::ConfigExtended));
    let held = holder.read(Space::PciConfig, host_bridge(0x44), Width::Byte).expect("acpi: a register past the header");
    assert_eq!(holder.write(Space::PciConfig, host_bridge(0x44), Width::Byte, held), Ok(()), "acpi: a write past the header of a function nothing drives");
    assert_eq!(holder.write(Space::PciConfig, pci_address(0, 0xFF, 31, 7, 0x44), Width::Byte, 0), Err(Refused::ConfigDriven), "acpi: a function this kernel did not enumerate");
    println!("acpi: the host bridge read as {id:#010x} by its address and through ECAM; its header and extended space refused every write, and one register past them took the value it held");
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

    // The firmware owns it: the take is refused and leaves its request.
    assert_eq!(syscall::debug_with(debug_action::ACPI_FIRMWARE_LOCK, 1), 0);
    assert_eq!(syscall::acpi_lock_take(holder.handle()), Ok(false), "acpi: a lock the firmware owns was taken");
    assert_eq!(word(holder), OWNED | PENDING);
    assert_eq!(syscall::acpi_lock_take(holder.handle()), Ok(false));
    assert_eq!(syscall::acpi_lock_release(holder.handle()), Err(SyscallError::InvalidArgument), "acpi: the firmware's lock was given back");
    assert_eq!(word(holder), OWNED | PENDING, "acpi: a refused release changed the word");
    // The firmware lets go, and the next take has it; the stale request goes with the take.
    assert_eq!(syscall::debug_with(debug_action::ACPI_FIRMWARE_LOCK, 0), OWNED | PENDING);
    assert_eq!(syscall::acpi_lock_take(holder.handle()), Ok(true));
    assert_eq!(word(holder), OWNED);
    assert_eq!(syscall::acpi_lock_release(holder.handle()), Ok(()));
    println!("acpi: the lock a dead holder left taken read free; it was taken and given back, and found pending while the firmware owned it");
}

/// What the keeper says once it holds the lock.
const KEPT: &str = "keeper: holding the Global Lock, and leaving with it";

fn keeper() {
    let claim: Device = Endowments::get().take(CLAIM_LABEL).expect("acpi: the keeper is endowed the claim");
    bind(&claim);
    assert_eq!(syscall::acpi_lock_take(claim.as_handle()), Ok(true), "keeper: the lock");
    println!("{KEPT}");
}
