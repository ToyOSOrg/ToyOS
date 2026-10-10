//! A HID-over-I2C touchpad behind an AMD FCH DesignWare I2C controller
//! (`AMDI0010`), read in mouse mode and fed to the pointer the PS/2 and USB
//! drivers feed. Measurement-only scaffolding for one AMD laptop's test
//! image: a kernel thread, which ToyOS does not ship, and never lands.
//!
//! The DSDT names the controller (`\_SB.I2CA`, 0xFEDC2000) and two
//! candidate touchpads behind it whose `_STA` the firmware decides at run
//! time: `TPD1` (PNP0C50 at 0x2C, HID descriptor register 0x20) and `TPD2`
//! (ELAN0634 at 0x15, HID descriptor register 0x01), each `_DSM` function 1
//! decoded out of tree. This kernel runs no AML, so it asks both addresses
//! and drives the one that answers.
//!
//! No interrupt is taken: the touchpad's line is GPIO 9 on the FCH's GPIO
//! bank (`AMDI0030`), whose level this reads every [`TICK`] and reads the
//! input register only while the line is away from the level it held idle.
//! Nothing here writes a chipset power, reset or pin-mux register, nor the
//! GPIO page, which the acpi claim's AML may write: a
//! controller that does not read as a DesignWare core is logged and left.

use toyos_i2chid::report::{self, Mouse};
use toyos_i2chid::{self as hid, HidDescriptor, Input};

use kernel::sched::task::WaitClass;

use crate::log;
use crate::mm::policy::MmioPolicy;
use crate::mm::Mmio;
use crate::mouse::{self, Motion, PointerSource};
use crate::time::{Deadline, Duration};
use crate::watch;

const I2CA: u64 = 0xFEDC_2000;
/// The FCH's GPIO bank and AOAC (always-on, always-connected) power
/// registers share this page.
const FCH_81: u64 = 0xFED8_1000;
const GPIO_BANK: u64 = 0x500;
const TOUCHPAD_PIN: u64 = 9;
/// AOAC's control and state bytes for I2C0, which `\_SB.I2CA.RSET` pulses.
const AOAC_I2C0: u64 = 0xE4A;

/// `(address, HID descriptor register)`, from each candidate's `_CRS` and `_DSM`.
const CANDIDATES: [(u8, u16); 2] = [(0x2C, 0x20), (0x15, 0x01)];

const TICK: u64 = 10;

/// The DesignWare core's registers (Synopsys DW_apb_i2c databook).
mod reg {
    pub const CON: u64 = 0x00;
    pub const TAR: u64 = 0x04;
    pub const DATA_CMD: u64 = 0x10;
    pub const SS_HCNT: u64 = 0x14;
    pub const SS_LCNT: u64 = 0x18;
    pub const FS_HCNT: u64 = 0x1C;
    pub const FS_LCNT: u64 = 0x20;
    pub const INTR_MASK: u64 = 0x30;
    pub const RAW_INTR_STAT: u64 = 0x34;
    pub const RX_TL: u64 = 0x38;
    pub const TX_TL: u64 = 0x3C;
    pub const CLR_INTR: u64 = 0x40;
    pub const CLR_TX_ABRT: u64 = 0x54;
    pub const CLR_STOP_DET: u64 = 0x60;
    pub const ENABLE: u64 = 0x6C;
    pub const STATUS: u64 = 0x70;
    pub const TXFLR: u64 = 0x74;
    pub const RXFLR: u64 = 0x78;
    pub const SDA_HOLD: u64 = 0x7C;
    pub const TX_ABRT_SOURCE: u64 = 0x80;
    pub const ENABLE_STATUS: u64 = 0x9C;
    pub const FS_SPKLEN: u64 = 0xA0;
    pub const COMP_PARAM_1: u64 = 0xF4;
    pub const COMP_VERSION: u64 = 0xF8;
    pub const COMP_TYPE: u64 = 0xFC;

    pub const DW_COMP_TYPE: u32 = 0x4457_0140;
    pub const CMD_READ: u32 = 1 << 8;
    pub const CMD_STOP: u32 = 1 << 9;
    pub const CMD_RESTART: u32 = 1 << 10;
    pub const INTR_TX_ABRT: u32 = 1 << 6;
    pub const INTR_STOP_DET: u32 = 1 << 9;
}

/// Is the DSDT's `_HID` string for the controller anywhere in it?
fn named_in_dsdt(rsdp_addr: u64) -> bool {
    let phys = crate::drivers::acpi::direct_phys();
    let Ok(mut blocks) = toyos_acpi::definition_blocks(phys, rsdp_addr) else { return false };
    let Some(Ok(dsdt)) = blocks.next() else { return false };
    let bytes: alloc::vec::Vec<u8> = (0..dsdt.len()).map_while(|i| dsdt.byte(i)).collect();
    bytes.windows(8).any(|w| w == b"AMDI0010")
}

/// Spawn the poller where the DSDT names an AMD I2C controller.
pub fn start(rsdp_addr: u64) {
    if !named_in_dsdt(rsdp_addr) {
        log!("i2c-hid: no AMDI0010 in the DSDT; no touchpad");
        return;
    }
    crate::sched::kthread::spawn("i2chid", body, 0);
}

struct Sleeper {
    parkable: crate::scheduler::Parkable,
    handle: alloc::sync::Arc<crate::sched::payload::TaskHandle>,
}

impl Sleeper {
    fn ms(&self, ms: u64) {
        let armed = watch::arm(self.handle.watch(), 0, WaitClass::Other).expect("i2chid runs as a task");
        let deadline = Deadline::at(crate::clock::now() + Duration::from_millis(ms));
        while !deadline.reached(crate::clock::now()) {
            watch::wait_uncancellable(&self.parkable, &armed, deadline);
        }
    }

    fn forever(&self) -> ! {
        loop {
            let armed = watch::arm(self.handle.watch(), 0, WaitClass::Other).expect("i2chid runs as a task");
            watch::wait_uncancellable(&self.parkable, &armed, Deadline::never());
        }
    }
}

extern "C" fn body(_arg: u64) -> ! {
    let sleep = Sleeper {
        parkable: crate::scheduler::Parkable::at_entry(),
        handle: crate::sched::driver::current_handle().expect("i2chid runs as a task"),
    };
    match bring_up(&sleep) {
        Ok(mut pad) => loop {
            pad.tick();
            sleep.ms(TICK);
        },
        Err(why) => {
            log!("i2c-hid: stopped: {why}");
            sleep.forever();
        }
    }
}

enum Fault {
    /// TX_ABRT_SOURCE as the abort left it.
    Abort(u32),
    Timeout { raw: u32, status: u32, txflr: u32, rxflr: u32, got: usize },
    EnableStuck(u32),
}

impl core::fmt::Display for Fault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            // Bit 0 is ADDR7_NOACK: nothing at the address.
            Self::Abort(source) => write!(f, "aborted, TX_ABRT_SOURCE {source:#x}"),
            Self::Timeout { raw, status, txflr, rxflr, got } => write!(
                f,
                "timed out: raw_intr {raw:#x} status {status:#x} txflr {txflr} rxflr {rxflr}, {got} bytes in"
            ),
            Self::EnableStuck(status) => write!(f, "IC_ENABLE_STATUS stuck at {status:#x}"),
        }
    }
}

struct Dw {
    mmio: Mmio,
    tx_depth: u32,
    rx_depth: u32,
}

impl Dw {
    fn r(&self, at: u64) -> u32 {
        self.mmio.read_u32(at)
    }

    fn w(&self, at: u64, v: u32) {
        self.mmio.write_u32(at, v)
    }

    fn set_enabled(&self, on: bool) -> Result<(), Fault> {
        self.w(reg::ENABLE, on as u32);
        let until = crate::clock::nanos_since_boot() + 25_000_000;
        while self.r(reg::ENABLE_STATUS) & 1 != on as u32 {
            if crate::clock::nanos_since_boot() > until {
                return Err(Fault::EnableStuck(self.r(reg::ENABLE_STATUS)));
            }
            core::hint::spin_loop();
        }
        Ok(())
    }

    /// One transaction to `addr`: `write`, then (after a repeated START)
    /// `read.len()` bytes, then STOP.
    fn transfer(&self, addr: u8, write: &[u8], read: &mut [u8]) -> Result<(), Fault> {
        self.set_enabled(false)?;
        self.w(reg::TAR, addr as u32);
        self.set_enabled(true)?;
        let _ = self.r(reg::CLR_INTR);
        let total = write.len() + read.len();
        let cmd = |i: usize| -> u32 {
            let stop = if i + 1 == total { reg::CMD_STOP } else { 0 };
            if i < write.len() {
                write[i] as u32 | stop
            } else {
                let restart = if i == write.len() && !write.is_empty() { reg::CMD_RESTART } else { 0 };
                reg::CMD_READ | restart | stop
            }
        };
        // Generous: 400 kHz moves a byte in 23 us.
        let until = crate::clock::nanos_since_boot() + 20_000_000 + total as u64 * 100_000;
        let (mut sent, mut got) = (0usize, 0usize);
        let result = loop {
            let raw = self.r(reg::RAW_INTR_STAT);
            if raw & reg::INTR_TX_ABRT != 0 {
                let source = self.r(reg::TX_ABRT_SOURCE);
                let _ = self.r(reg::CLR_TX_ABRT);
                break Err(Fault::Abort(source));
            }
            while got < read.len() && self.r(reg::RXFLR) > 0 {
                read[got] = self.r(reg::DATA_CMD) as u8;
                got += 1;
            }
            while sent < total && self.r(reg::TXFLR) < self.tx_depth {
                // A read issued is a byte the RX FIFO must hold until drained.
                if sent >= write.len() && sent - write.len() - got >= self.rx_depth as usize {
                    break;
                }
                self.w(reg::DATA_CMD, cmd(sent));
                sent += 1;
            }
            if sent == total && got == read.len() && raw & reg::INTR_STOP_DET != 0 {
                break Ok(());
            }
            if crate::clock::nanos_since_boot() > until {
                break Err(Fault::Timeout {
                    raw,
                    status: self.r(reg::STATUS),
                    txflr: self.r(reg::TXFLR),
                    rxflr: self.r(reg::RXFLR),
                    got,
                });
            }
            core::hint::spin_loop();
        };
        let _ = self.r(reg::CLR_STOP_DET);
        self.set_enabled(false)?;
        result
    }
}

struct Pad {
    dw: Dw,
    addr: u8,
    mouse: Mouse,
    source: PointerSource,
    fch: Mmio,
    idle_level: bool,
    buf: alloc::vec::Vec<u8>,
    ticks: u64,
    logged: u32,
    blind_logged: u32,
    faults: u32,
}

fn pin(fch: Mmio) -> u32 {
    fch.read_u32(GPIO_BANK + TOUCHPAD_PIN * 4)
}

fn level(raw: u32) -> bool {
    raw & (1 << 16) != 0
}

fn hex(bytes: &[u8]) -> impl core::fmt::Display + '_ {
    struct Hex<'a>(&'a [u8]);
    impl core::fmt::Display for Hex<'_> {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            for b in self.0 {
                write!(f, "{b:02x}")?;
            }
            Ok(())
        }
    }
    Hex(bytes)
}

fn bring_up(sleep: &Sleeper) -> Result<Pad, &'static str> {
    let mmio = crate::mm::paging::map_mmio(I2CA, 0x1000, MmioPolicy::Uncacheable);
    // Only read, so reached through I2CA's 2 MiB mapping rather than mapped
    // as a window this kernel drives, which would refuse the acpi claim's
    // AML every GPIO and AOAC field on the page.
    const { assert!(FCH_81 >> 21 == I2CA >> 21) };
    let fch = Mmio::new(crate::mm::DirectMap::from_phys(FCH_81), 0x1000);
    log!(
        "i2c-hid: AOAC I2C0 control={:#04x} state={:#04x}; GPIO{} {:#010x}",
        fch.read_u8(AOAC_I2C0),
        fch.read_u8(AOAC_I2C0 + 1),
        TOUCHPAD_PIN,
        pin(fch)
    );
    let comp_type = mmio.read_u32(reg::COMP_TYPE);
    let version = mmio.read_u32(reg::COMP_VERSION);
    let param = mmio.read_u32(reg::COMP_PARAM_1);
    log!("i2c-hid: I2CA {I2CA:#x} comp_type={comp_type:#010x} version={version:#010x} param_1={param:#010x}");
    if comp_type != reg::DW_COMP_TYPE {
        log!(
            "i2c-hid: I2CA does not read as a DesignWare core: powered off or not decoded. Its _STA reads a \
             firmware NVS byte, it has no _PS0, and RSET pulses AOAC bits 7:6 at {:#x}; none of that is done here",
            FCH_81 + AOAC_I2C0
        );
        return Err("no DesignWare core at I2CA");
    }
    log!(
        "i2c-hid: firmware left con={:#x} enable={:#x} ss={}/{} fs={}/{} sda_hold={:#x} fs_spklen={} status={:#x} tar={:#x}",
        mmio.read_u32(reg::CON),
        mmio.read_u32(reg::ENABLE),
        mmio.read_u32(reg::SS_HCNT),
        mmio.read_u32(reg::SS_LCNT),
        mmio.read_u32(reg::FS_HCNT),
        mmio.read_u32(reg::FS_LCNT),
        mmio.read_u32(reg::SDA_HOLD),
        mmio.read_u32(reg::FS_SPKLEN),
        mmio.read_u32(reg::STATUS),
        mmio.read_u32(reg::TAR)
    );
    let dw = Dw { mmio, tx_depth: ((param >> 16) & 0xFF) + 1, rx_depth: ((param >> 8) & 0xFF) + 1 };
    let fast = (param >> 2) & 3 >= 2;
    dw.set_enabled(false).map_err(|_| "I2CA would not disable")?;
    // Linux's counts for this ACPI id's 150 MHz clock (acpi_apd.c), with its
    // default 300 ns fall time: 100 kHz standard, about 350 kHz fast.
    dw.w(reg::SS_HCNT, 642);
    dw.w(reg::SS_LCNT, 749);
    dw.w(reg::FS_HCNT, 132);
    dw.w(reg::FS_LCNT, 239);
    // A core from 1.11a holds SDA for at least one clock on receive, as Linux sets it.
    let hold = dw.r(reg::SDA_HOLD);
    if version >= 0x3131_312A && hold & 0x00FF_0000 == 0 {
        dw.w(reg::SDA_HOLD, hold | 1 << 16);
    }
    // Master, the speed, RESTART allowed, slave disabled.
    dw.w(reg::CON, 0x01 | if fast { 2 << 1 } else { 1 << 1 } | 1 << 5 | 1 << 6);
    dw.w(reg::INTR_MASK, 0);
    dw.w(reg::RX_TL, 0);
    dw.w(reg::TX_TL, 0);
    log!(
        "i2c-hid: master at {} kHz, fifo tx={} rx={}, con={:#x}",
        if fast { 350 } else { 100 },
        dw.tx_depth,
        dw.rx_depth,
        dw.r(reg::CON)
    );

    let mut found = None;
    for (addr, register) in CANDIDATES {
        let mut d = [0u8; hid::HID_DESCRIPTOR_LEN];
        match dw.transfer(addr, &register.to_le_bytes(), &mut d) {
            Ok(()) => {
                log!("i2c-hid: {addr:#04x} answered, HID descriptor at {register:#x}: {}", hex(&d));
                match HidDescriptor::parse(&d) {
                    Ok(desc) => {
                        found = Some((addr, desc));
                        break;
                    }
                    Err(why) => log!("i2c-hid: {addr:#04x}'s HID descriptor refused: {why:?}"),
                }
            }
            Err(why) => log!("i2c-hid: {addr:#04x} did not answer: {why}"),
        }
    }
    let (addr, desc) = found.ok_or("no candidate answered with a HID descriptor")?;
    log!("i2c-hid: {addr:#04x} {desc:x?}");

    if desc.report_descriptor_len as usize > 4096 {
        return Err("a report descriptor past 4 KiB");
    }
    let mut rd = alloc::vec![0u8; desc.report_descriptor_len as usize];
    dw.transfer(addr, &desc.report_descriptor_register.to_le_bytes(), &mut rd).map_err(|why| {
        log!("i2c-hid: report descriptor read failed: {why}");
        "report descriptor read failed"
    })?;
    log!("i2c-hid: report descriptor, {} bytes:", rd.len());
    for (n, chunk) in rd.chunks(64).enumerate() {
        log!("i2c-hid: rd[{:04x}] {}", n * 64, hex(chunk));
    }
    let mouse = report::mouse(&rd).map_err(|why| {
        log!("i2c-hid: no mouse collection: {why:?}");
        "no mouse collection"
    })?;
    log!("i2c-hid: mouse collection {mouse:?}");

    for (what, opcode) in [("SET_POWER ON", hid::SET_POWER), ("RESET", hid::RESET)] {
        let c = hid::command(desc.command_register, opcode, hid::POWER_ON);
        dw.transfer(addr, &c, &mut []).map_err(|why| {
            log!("i2c-hid: {what} failed: {why}");
            "a command failed"
        })?;
        log!("i2c-hid: {what} sent ({}); GPIO{TOUCHPAD_PIN} {:#010x}", hex(&c), pin(fch));
        sleep.ms(TICK);
    }

    let max = (desc.max_input_len as usize).clamp(mouse.read_len(), 512);
    let mut buf = alloc::vec![0u8; max];
    // The reset's answer is a zero length (§7.2.1); a level-triggered line
    // stays asserted until it is read, so read until the device has nothing.
    let mut drained = 0;
    for attempt in 0..100 {
        let before = pin(fch);
        match dw.transfer(addr, &[], &mut buf) {
            Ok(()) => {
                let input = hid::input(&buf);
                log!("i2c-hid: drain {attempt}: GPIO{TOUCHPAD_PIN} {before:#010x}, read {} -> {input:?}", hex(&buf[..buf.len().min(16)]));
                if input == Input::Nothing {
                    drained += 1;
                    if drained == 2 {
                        break;
                    }
                }
            }
            Err(why) => log!("i2c-hid: drain {attempt} failed: {why}"),
        }
        sleep.ms(TICK);
    }
    let idle = pin(fch);
    log!("i2c-hid: idle GPIO{TOUCHPAD_PIN} {idle:#010x}: a report is read while its level differs");

    let source = PointerSource::claim().ok_or("every pointer source is taken")?;
    mouse::declare_source();
    log!("i2c-hid: touchpad {addr:#04x} {:04x}:{:04x} is pointer source {}", desc.vendor, desc.product, source.id());
    Ok(Pad {
        dw,
        addr,
        mouse,
        source,
        fch,
        idle_level: level(idle),
        buf,
        ticks: 0,
        logged: 0,
        blind_logged: 0,
        faults: 0,
    })
}

impl Pad {
    fn tick(&mut self) {
        self.ticks += 1;
        let raw = pin(self.fch);
        let asserted = level(raw) != self.idle_level;
        // Once a second for the first minute, a read the line did not ask
        // for, logged and never applied: what this device answers unasked.
        let blind = !asserted && self.ticks.is_multiple_of(100) && self.blind_logged < 60;
        if !asserted && !blind {
            return;
        }
        if let Err(why) = self.dw.transfer(self.addr, &[], &mut self.buf) {
            self.faults += 1;
            if self.faults.is_power_of_two() {
                log!("i2c-hid: input read failed ({} so far): {why}", self.faults);
            }
            return;
        }
        let input = hid::input(&self.buf);
        if blind {
            self.blind_logged += 1;
            if input != Input::Nothing {
                log!("i2c-hid: unasked read (GPIO {raw:#010x}): {input:x?}");
            }
            return;
        }
        let Input::Report(r) = input else {
            if self.logged < 64 {
                self.logged += 1;
                log!("i2c-hid: line asserted (GPIO {raw:#010x}), read {input:x?}");
            }
            return;
        };
        let read = self.mouse.read(r);
        if self.logged < 64 {
            self.logged += 1;
            log!("i2c-hid: report {} -> {read:?}", hex(r));
        }
        if let Ok(m) = read {
            let wheel = m.wheel.clamp(i8::MIN as i32, i8::MAX as i32) as i8;
            if mouse::handle_motion(self.source, m.buttons, Motion::Relative { dx: m.dx, dy: m.dy }, wheel) {
                mouse::WATCH.post();
            }
        }
    }
}
