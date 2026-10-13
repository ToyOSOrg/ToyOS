//! The AMD microcode scout: `kernel::amducode::ladder` run on this machine's
//! CPUs, one log line per reading. Armed by `amd-ucode-scout`; a measuring
//! instrument, not a loader.
//!
//! It runs on the BSP once `smp::boot_aps` has every AP up, each AP waiting
//! in [`station`] between its echo and the idle loop: the BSP posts one order
//! at a time and an AP carries out the ones addressed to it, so no two CPUs
//! write the loader at once and every reading comes from the CPU it names.
//! The APs leave the station when the ladder ends, however it ends.
//!
//! **Every write is guarded.** A #GP raised by the `wrmsr` in
//! [`wrmsr_or_fault`] is answered by [`caught`] from `trap_dispatch`, which
//! resumes past it with the error code, so a refused write is a reading and
//! never a crash. The level is read after every write.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::arch::naked_asm;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::{Acquire, Relaxed, Release}};

use kernel::amducode::container;
use kernel::amducode::ladder::{self, Bench, Bytes, Cpu, Lost, Placement, Reading, Staged, Write};
use kernel::amducode::smbios::{self, Quoted};
use toyos_acpi::Phys;

use super::idt::TrapFrame;
use super::{apic, cpu, hw, percpu, IrqGuard};
use crate::smp_roster::MAX_CPUS;
use crate::{clock, log, smp};

/// MSR0000_008B, `PATCH_LEVEL` (55922 §2.1.14.1): per core, Error-on-write.
const PATCH_LEVEL: u32 = 0x8b;
/// MSRC001_0020, `PATCH_LOADER` (57896 §2.1.13.3, 55901 §2.1.15.3; not
/// documented for Family 17h): written the linear address of a patch block.
const PATCH_LOADER: u32 = 0xc001_0020;

const FILE_LEN: usize = 22_596;
/// Puts the bench's block, 0x262c into the file, at page offset 0x00c: 4-byte
/// aligned and not 16, wholly inside one page, so M5's in-place load varies
/// the alignment alone.
const LEAD: usize = 0x9e0;

#[repr(C, align(4096))]
struct Placed {
    _lead: [u8; LEAD],
    file: [u8; FILE_LEN],
}

/// AMD's Family 17h container, pinned by its digest in `NOTICE`.
static PLACED: Placed = Placed {
    _lead: [0; LEAD],
    file: *include_bytes!("../../../microcode/amd-ucode/microcode_amd_fam17h.bin"),
};

/// Two pages a block is copied into before its address is posted.
#[repr(C, align(4096))]
struct Scratch([u8; 2 * 4096]);

/// Where a straddling copy starts: 16-byte aligned, half its 3200 bytes on
/// each side of the page boundary.
const STRADDLE_AT: usize = 4096 - 1600;
/// A byte of patch data past any reading of the header, flipped for M2.
const BODY_BYTE: usize = 0x640;
const LEVEL_BYTE: usize = 0x04;

/// How long an AP has to carry out an order: a load is microseconds.
const ANSWER_NS: u64 = 100_000_000;
/// How long every AP but the BSP halts before M6 reads the levels: the span
/// measured, not a wait for an event.
const IDLE_NS: u64 = 2_000_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    Level = 1,
    Load = 2,
    WriteLevel = 3,
    Idle = 4,
    Leave = 5,
}

/// An order's number, target and kind in one word, so a CPU never reads one
/// order's target with another's kind.
#[derive(Clone, Copy)]
struct Posted {
    seq: u32,
    /// A CPU, or [`EVERY_AP`].
    target: u32,
    order: Order,
}

/// Every AP, as [`Posted::target`] carries it in 24 bits.
const EVERY_AP: u32 = 0xff_ffff;

impl Posted {
    fn word(self) -> u64 {
        u64::from(self.seq) << 32 | u64::from(self.target) << 8 | self.order as u64
    }

    /// `None` before the first order.
    fn of(word: u64) -> Option<Self> {
        let order = match word as u8 {
            1 => Order::Level,
            2 => Order::Load,
            3 => Order::WriteLevel,
            4 => Order::Idle,
            5 => Order::Leave,
            _ => return None,
        };
        Some(Self { seq: (word >> 32) as u32, target: (word >> 8) as u32 & EVERY_AP, order })
    }
}

/// One order at a time, and its answer.
struct Mail {
    /// The [`Posted`] word, stored by the BSP last, with release.
    posted: AtomicU64,
    /// A [`Order::Load`]'s address, stable until its target answers.
    at: AtomicU64,
    /// The `seq` the target answered, stored last with release.
    answered: AtomicU32,
    before: AtomicU32,
    after: AtomicU32,
    /// The #GP's error code, or [`NO_FAULT`].
    fault: AtomicU64,
    ticks: AtomicU64,
    /// One bit per AP halted in an [`Order::Idle`].
    halted: AtomicU32,
}

const _: () = assert!(MAX_CPUS < 32, "`Mail::halted` holds one bit per CPU");
const NO_FAULT: u64 = u64::MAX;

static MAIL: Mail = Mail {
    posted: AtomicU64::new(0),
    at: AtomicU64::new(0),
    answered: AtomicU32::new(0),
    before: AtomicU32::new(0),
    after: AtomicU32::new(0),
    fault: AtomicU64::new(NO_FAULT),
    ticks: AtomicU64::new(0),
    halted: AtomicU32::new(0),
};

/// Each CPU's core, CPUID Fn8000_001E EBX[7:0], as it read it on arrival.
static CORES: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(UNREAD) }; MAX_CPUS];
const UNREAD: u32 = u32::MAX;
/// Set once the ladder has ended; every AP leaves the station on it too.
static LEFT: AtomicBool = AtomicBool::new(false);

/// An AP between its echo and the idle loop, carrying out the BSP's orders
/// until the ladder ends.
pub fn station() {
    let me = percpu::cpu_id();
    if let Some(slot) = CORES.get(me as usize) {
        slot.store(core_id().unwrap_or(UNREAD), Release);
    }
    let mut seen = 0;
    while !LEFT.load(Acquire) {
        let Some(posted) = Posted::of(MAIL.posted.load(Acquire)).filter(|p| p.seq != seen) else {
            core::hint::spin_loop();
            continue;
        };
        seen = posted.seq;
        match posted.order {
            Order::Leave => break,
            Order::Idle => halt_until_next(me, posted.seq),
            _ if posted.target != me => {}
            Order::Level => {
                MAIL.before.store(level(), Relaxed);
                MAIL.answered.store(posted.seq, Release);
            }
            order @ (Order::Load | Order::WriteLevel) => {
                let write = match order {
                    Order::Load => write_measured(PATCH_LOADER, MAIL.at.load(Relaxed)),
                    _ => write_measured(PATCH_LEVEL, 0),
                };
                MAIL.before.store(write.before, Relaxed);
                MAIL.after.store(write.after, Relaxed);
                MAIL.fault.store(write.fault.map_or(NO_FAULT, u64::from), Relaxed);
                MAIL.ticks.store(write.ticks, Relaxed);
                MAIL.answered.store(posted.seq, Release);
            }
        }
    }
}

/// `sti; hlt` until an order after `seq` is posted; the BSP kicks every AP
/// when it posts the next.
fn halt_until_next(me: u32, seq: u32) {
    MAIL.halted.fetch_or(1 << me, Release);
    while Posted::of(MAIL.posted.load(Acquire)).is_some_and(|p| p.seq == seq) {
        hw::halt();
        cpu::disable_interrupts();
    }
}

/// The BSP's half: the readings, then the APs released.
pub fn run(smbios_addr: u64) {
    firmware(smbios_addr);
    ladder_if_patch();
    LEFT.store(true, Release);
    post(EVERY_AP, Order::Leave, 0);
    log!("amducode: the APs leave the station");
}

/// M10: which part this is and which firmware it runs.
fn firmware(smbios_addr: u64) {
    let (signature, ..) = cpu::cpuid(1, 0);
    log!("amducode: M10 cpu0 signature {signature:#x} brand {}", Quoted(brand().as_deref()));
    let table = match smbios_table(smbios_addr) {
        Ok(table) => table,
        Err(why) => {
            log!("amducode: M10 smbios unread: {why}");
            return;
        }
    };
    let (entry, bytes) = table;
    match smbios::bios(&bytes) {
        Some(bios) => log!(
            "amducode: M10 smbios {}.{} bios vendor {} version {} date {} release {} ec {}",
            entry.major,
            entry.minor,
            Quoted(bios.vendor),
            Quoted(bios.version),
            Quoted(bios.date),
            Revision(bios.release),
            Revision(bios.ec),
        ),
        None => log!("amducode: M10 smbios {}.{} has no BIOS information structure", entry.major, entry.minor),
    }
    let mut named = 0;
    for version in smbios::platform_versions(&bytes) {
        named += 1;
        log!("amducode: M10 platform {}", Quoted(Some(version)));
    }
    if named == 0 {
        log!("amducode: M10 platform none named in SMBIOS");
    }
}

/// A major and minor release, or `none` where the structure is too old to say.
struct Revision(Option<(u8, u8)>);

impl core::fmt::Display for Revision {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some((major, minor)) => write!(f, "{major}.{minor}"),
            None => f.write_str("none"),
        }
    }
}

/// CPUID Fn8000_0002–0004, without its trailing NULs and spaces.
fn brand() -> Option<Vec<u8>> {
    if cpu::cpuid(0x8000_0000, 0).0 < 0x8000_0004 {
        return None;
    }
    let mut bytes = Vec::with_capacity(48);
    for leaf in 0x8000_0002..=0x8000_0004 {
        let (a, b, c, d) = cpu::cpuid(leaf, 0);
        for word in [a, b, c, d] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
    }
    while bytes.last().is_some_and(|&b| b == 0 || b == b' ') {
        bytes.pop();
    }
    Some(bytes)
}

/// The entry point at `addr` and a copy of the structure table it names.
fn smbios_table(addr: u64) -> Result<(smbios::EntryPoint, Vec<u8>), &'static str> {
    if addr == 0 {
        return Err("the loader was handed no entry point");
    }
    let phys = crate::drivers::acpi::direct_phys();
    let read = |at: u64, len: usize| -> Result<Vec<u8>, &'static str> {
        if !phys.readable(at, len) {
            return Err("it lies outside the direct map");
        }
        Ok((0..len as u64).map(|i| phys.byte(at + i)).collect())
    };
    let entry = smbios::entry_point(&read(addr, smbios::ENTRY_POINT_MAX)?)?;
    Ok((entry, read(entry.table, entry.len as usize)?))
}

fn ladder_if_patch() {
    let vendor = cpu::vendor();
    if vendor != toyos_cpuvuln::Vendor::Amd {
        log!("amducode: stop: the CPU is {vendor:?}, and the scout measures AMD's loader");
        return;
    }
    let (signature, ..) = cpu::cpuid(1, 0);
    let patch = match container::patch_for(&PLACED.file, signature) {
        Ok(Some(patch)) => patch,
        Ok(None) => {
            log!("amducode: stop: AMD's Family 17h container holds no patch for signature {signature:#x}");
            return;
        }
        Err(why) => panic!("amducode: AMD's pinned container is refused: {why}"),
    };
    log!(
        "amducode: patch level {:#x} equivalence {:#x} at {:p}, container byte {:#x}, {} bytes",
        patch.level,
        patch.equivalence,
        patch.block.as_ptr(),
        patch.offset,
        patch.block.len()
    );
    let Some(cpus) = cpus() else { return };
    let scratch = Box::new(Scratch([0; 2 * 4096]));
    assert!(scratch.0.as_ptr().addr().is_multiple_of(4096), "amducode: the scratch pages are not page-aligned");
    let mut machine = Machine { cpus, block: patch.block, scratch };
    ladder::run(&mut machine, patch.level);
}

/// Every CPU online with its core, or `None` said why.
fn cpus() -> Option<Vec<Cpu>> {
    let count = smp::cpu_count();
    CORES[0].store(core_id().unwrap_or(UNREAD), Relaxed);
    let mut cpus = Vec::new();
    for id in 0..count {
        let slot = &CORES[id as usize];
        if !clock::settles(ANSWER_NS, || slot.load(Acquire) != UNREAD) {
            log!("amducode: stop: cpu{id} names no core (CPUID Fn8000_001E) or never reached the station");
            return None;
        }
        cpus.push(Cpu { id, apic: smp::hardware_id(id), core: slot.load(Acquire) });
    }
    Some(cpus)
}

/// CPUID Fn8000_001E EBX[7:0], CoreId, where TopologyExtensions says the
/// leaf is there.
fn core_id() -> Option<u32> {
    const TOPOLOGY_EXTENSIONS: u32 = 1 << 22;
    let has = cpu::cpuid(0x8000_0000, 0).0 >= 0x8000_001e && cpu::cpuid(0x8000_0001, 0).2 & TOPOLOGY_EXTENSIONS != 0;
    has.then(|| cpu::cpuid(0x8000_001e, 0).1 & 0xff)
}

struct Machine<'a> {
    cpus: Vec<Cpu>,
    block: &'a [u8],
    /// Written only between orders: the CPU handed its address has answered
    /// before the next copy.
    scratch: Box<Scratch>,
}

impl Machine<'_> {
    /// The address of `staged`'s bytes, copied into the scratch pages unless
    /// in place.
    fn stage(&mut self, staged: Staged) -> u64 {
        let offset = match staged.placement {
            Placement::InPlace => {
                assert!(staged.bytes == Bytes::Intact, "amducode: the container in place is AMD's bytes alone");
                return self.block.as_ptr() as u64;
            }
            Placement::Aligned => 0,
            Placement::Straddling => STRADDLE_AT,
        };
        let copy = &mut self.scratch.0[offset..offset + self.block.len()];
        copy.copy_from_slice(self.block);
        match staged.bytes {
            Bytes::Intact => {}
            Bytes::BodyFlipped => copy[BODY_BYTE] ^= 1,
            Bytes::LevelFlipped => copy[LEVEL_BYTE] ^= 1,
        }
        copy.as_ptr() as u64
    }

    /// `msr` written `value` on `cpu`, here or by its station.
    fn write(&mut self, cpu: u32, msr: u32, value: u64) -> Result<Write, Lost> {
        if cpu == 0 {
            return Ok(write_measured(msr, value));
        }
        let order = if msr == PATCH_LOADER { Order::Load } else { Order::WriteLevel };
        let seq = post(cpu, order, value);
        answer(cpu, seq)?;
        let ticks = MAIL.ticks.load(Relaxed);
        Ok(Write {
            at: if msr == PATCH_LOADER { value } else { 0 },
            before: MAIL.before.load(Relaxed),
            fault: match MAIL.fault.load(Relaxed) {
                NO_FAULT => None,
                code => Some(code as u32),
            },
            after: MAIL.after.load(Relaxed),
            ticks,
            nanos: clock::nanos_of_ticks(ticks),
        })
    }
}

impl Bench for Machine<'_> {
    fn cpus(&self) -> &[Cpu] {
        &self.cpus
    }

    fn level(&mut self, cpu: u32) -> Result<u32, Lost> {
        if cpu == 0 {
            return Ok(level());
        }
        let seq = post(cpu, Order::Level, 0);
        answer(cpu, seq)?;
        Ok(MAIL.before.load(Relaxed))
    }

    fn load(&mut self, cpu: u32, staged: Staged) -> Result<Write, Lost> {
        let at = self.stage(staged);
        self.write(cpu, PATCH_LOADER, at)
    }

    fn write_level(&mut self, cpu: u32) -> Result<Write, Lost> {
        self.write(cpu, PATCH_LEVEL, 0)
    }

    fn idle(&mut self) -> Result<u64, Lost> {
        let aps = (1u32 << smp::cpu_count()) - 2;
        MAIL.halted.store(0, Relaxed);
        post(EVERY_AP, Order::Idle, 0);
        clock::settles(ANSWER_NS, || MAIL.halted.load(Acquire) == aps);
        let missing = aps & !MAIL.halted.load(Acquire);
        if missing != 0 {
            return Err(Lost(missing.trailing_zeros()));
        }
        let began = clock::nanos_since_boot();
        clock::settles(IDLE_NS, || false);
        Ok((clock::nanos_since_boot() - began) / 1_000_000)
    }

    fn say(&mut self, reading: Reading) {
        log!("{reading}");
    }
}

/// Posts `order` for `target`, kicking the APs out of the halt an
/// [`Order::Idle`] before it left them in; its `seq`.
fn post(target: u32, order: Order, at: u64) -> u32 {
    let last = Posted::of(MAIL.posted.load(Relaxed));
    let seq = last.map_or(1, |p| p.seq.wrapping_add(1));
    MAIL.at.store(at, Relaxed);
    MAIL.posted.store(Posted { seq, target, order }.word(), Release);
    if last.is_some_and(|p| p.order == Order::Idle) {
        for cpu in 1..smp::cpu_count() {
            apic::kick_cpu(cpu);
        }
    }
    seq
}

fn answer(cpu: u32, seq: u32) -> Result<(), Lost> {
    match clock::settles(ANSWER_NS, || MAIL.answered.load(Acquire) == seq) {
        true => Ok(()),
        false => Err(Lost(cpu)),
    }
}

/// MSR0000_008B, bits 31:0.
fn level() -> u32 {
    cpu::rdmsr(PATCH_LEVEL) as u32
}

/// `msr` written `value` on this CPU with interrupts off: the level before,
/// the write and its ticks, CPUID leaf 0, the level after.
fn write_measured(msr: u32, value: u64) -> Write {
    let _closed = IrqGuard::close();
    let before = level();
    let began = cpu::counter();
    // SAFETY: PATCH_LOADER is handed the address of a whole block the kernel
    // maps and leaves untouched until this returns; PATCH_LEVEL is
    // Error-on-write on AMD and holds nothing the kernel relies on. A #GP is
    // caught.
    let result = unsafe { wrmsr_or_fault(msr, value) };
    let ticks = cpu::counter().wrapping_sub(began);
    cpu::cpuid(0, 0);
    Write {
        at: if msr == PATCH_LOADER { value } else { 0 },
        before,
        fault: (result >> 63 != 0).then_some(result as u32),
        after: level(),
        ticks,
        nanos: clock::nanos_of_ticks(ticks),
    }
}

unsafe extern "C" {
    /// The guarded `wrmsr`, and where its #GP resumes.
    static toyos_amducode_wrmsr: u8;
    static toyos_amducode_resume: u8;
}

/// `wrmsr` of `value` to `msr`: 0, or bit 63 set over the error code of the
/// #GP it raised.
///
/// # Safety
/// The write is one the machine survives; a #GP is caught, nothing else is.
#[unsafe(naked)]
unsafe extern "sysv64" fn wrmsr_or_fault(msr: u32, value: u64) -> u64 {
    naked_asm!(
        "mov ecx, edi",
        "mov eax, esi",
        "mov rdx, rsi",
        "shr rdx, 32",
        ".global toyos_amducode_wrmsr",
        "toyos_amducode_wrmsr:",
        "wrmsr",
        "xor eax, eax",
        "ret",
        ".global toyos_amducode_resume",
        "toyos_amducode_resume:",
        "mov rax, rdx",
        "bts rax, 63",
        "ret",
    );
}

/// A Ring 0 #GP at the guarded `wrmsr`, resumed past it with its error code
/// in `rdx`; `false` for any other.
pub fn caught(frame: &mut TrapFrame) -> bool {
    let at = &raw const toyos_amducode_wrmsr as u64;
    if toyos_userbound::Ring::of_cs(frame.cs).is_user() || frame.rip != at {
        return false;
    }
    frame.rip = &raw const toyos_amducode_resume as u64;
    frame.rdx = frame.error_code;
    true
}
