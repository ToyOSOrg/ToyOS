//! CMOS real-time clock: decoded once per boot into a [`Result`], never a
//! panic, because a read can be absent, wedged, or torn mid-update.
//! [`read`] takes the six-register set twice and accepts only two matching
//! reads, since a mid-update read is the only way two reads can disagree.

use core::fmt;

use crate::arch::cpu;
use crate::clock;
use crate::time::{Bound, Duration};
use toyos_wallclock::Civil;

const CMOS_ADDR: u16 = 0x70;
const CMOS_DATA: u16 = 0x71;

const SECONDS: u8 = 0x00;
const MINUTES: u8 = 0x02;
const HOURS: u8 = 0x04;
const DAY: u8 = 0x07;
const MONTH: u8 = 0x08;
const YEAR: u8 = 0x09;
const STATUS_A: u8 = 0x0A;
const STATUS_B: u8 = 0x0B;

/// Register A bit 7: the clock registers are mid-update and must not be read.
const UPDATE_IN_PROGRESS: u8 = 1 << 7;
/// Register B bit 1: hours run 0..=23 rather than 1..=12 with a PM flag.
const HOUR_24: u8 = 1 << 1;
/// Register B bit 2: the registers hold binary rather than BCD.
const BINARY: u8 = 1 << 2;
/// Bit 7 of the hours register, in 12-hour mode only.
const PM: u8 = 1 << 7;

/// Expiry is [`RtcFault::Updating`], not a retry or a panic.
/// 50x the hardware's own worst case: erring long only delays a boot whose
/// clock is already broken.
const MAX_UIP: Bound = Bound::from_spec(
    Duration::from_millis(100),
    "MC146818: the flag is raised 244us ahead of an update and cleared at most 1984us later",
);

/// Four: three disagreements in a row means the registers never held one
/// instant.
const MAX_READ_ATTEMPTS: u32 = 4;

/// Why this machine did not say what time it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtcFault {
    /// [`UPDATE_IN_PROGRESS`] never cleared inside [`MAX_UIP`].
    Updating,
    /// No two of [`MAX_READ_ATTEMPTS`] reads agreed.
    Unstable,
    /// The registers agreed on a value that is not a valid date.
    NotADate,
}

impl fmt::Display for RtcFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Updating => write!(
                f,
                "its update flag never cleared in {} ms, which is what an absent one looks like",
                MAX_UIP.duration().millis()
            ),
            Self::Unstable => write!(
                f,
                "no two of {MAX_READ_ATTEMPTS} reads agreed, so its registers never described one instant"
            ),
            Self::NotADate => write!(f, "its registers hold something that is not a date"),
        }
    }
}

/// Current time; `century_reg` is `None` when firmware names no century
/// register.
pub fn read(century_reg: Option<u8>) -> Result<Civil, RtcFault> {
    // Requires the monotonic clock already running: this bound is a duration.
    // An uncalibrated clock here is the kernel's own init order, not a
    // hardware fault, so this fails fast rather than returning one.
    assert!(clock::calibrated(), "rtc::read before the monotonic clock was calibrated");

    let mut previous = read_registers(century_reg)?;
    for _ in 1..MAX_READ_ATTEMPTS {
        let current = read_registers(century_reg)?;
        if current == previous {
            return decode(current);
        }
        previous = current;
    }
    Err(RtcFault::Unstable)
}

/// Equality between two reads is the evidence they describe one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Registers {
    sec: u8,
    min: u8,
    hour: u8,
    day: u8,
    month: u8,
    year: u8,
    /// `None` means no century register, distinct from one holding zero.
    century: Option<u8>,
    status_b: u8,
}

fn read_registers(century_reg: Option<u8>) -> Result<Registers, RtcFault> {
    wait_for_update()?;
    Ok(Registers {
        sec: cmos_read(SECONDS),
        min: cmos_read(MINUTES),
        hour: cmos_read(HOURS),
        day: cmos_read(DAY),
        month: cmos_read(MONTH),
        year: cmos_read(YEAR),
        century: century_reg.map(century_read),
        status_b: cmos_read(STATUS_B),
    })
}

/// Answers the *next* century, so a test can see the register's value reach
/// the target year.
fn century_read(reg: u8) -> u8 {
    if crate::actuator::rtc_century_next() { 0x21 } else { cmos_read(reg) }
}

fn wait_for_update() -> Result<(), RtcFault> {
    let deadline = clock::nanos_since_boot() + MAX_UIP.nanos();
    while cmos_read(STATUS_A) & UPDATE_IN_PROGRESS != 0 {
        if clock::nanos_since_boot() >= deadline {
            return Err(RtcFault::Updating);
        }
        core::hint::spin_loop();
    }
    Ok(())
}

/// Register B's format bits are read here; nothing past this point assumes
/// BCD or 12-hour.
fn decode(r: Registers) -> Result<Civil, RtcFault> {
    let binary = r.status_b & BINARY != 0;
    let field = |raw: u8| {
        if binary { Some(raw) } else { bcd_to_bin(raw) }.ok_or(RtcFault::NotADate)
    };

    let sec = field(r.sec)?;
    let min = field(r.min)?;
    let day = field(r.day)?;
    let month = field(r.month)?;
    let year_lo = field(r.year)?;

    let hour = if r.status_b & HOUR_24 != 0 {
        field(r.hour)?
    } else {
        let afternoon = r.hour & PM != 0;
        let twelve = field(r.hour & !PM)?;
        if !(1..=12).contains(&twelve) {
            return Err(RtcFault::NotADate);
        }
        twelve % 12 + if afternoon { 12 } else { 0 }
    };

    let year = match r.century {
        Some(raw) => {
            let century = field(raw)?;
            // Below 1900 the register is unmaintained; refuse rather than
            // fall back to the two-digit year.
            if !(19..=99).contains(&century) {
                return Err(RtcFault::NotADate);
            }
            century as u64 * 100 + year_lo as u64
        }
        // 2000, not an older pivot: this kernel only boots UEFI machines.
        None => 2000 + year_lo as u64,
    };

    let civil = Civil {
        year,
        month: month as u64,
        day: day as u64,
        hour: hour as u64,
        min: min as u64,
        sec: sec as u64,
    };
    if !civil.is_valid() {
        return Err(RtcFault::NotADate);
    }
    Ok(civil)
}

/// `None` for a nibble above nine, which is not a digit and so not a time.
fn bcd_to_bin(bcd: u8) -> Option<u8> {
    let (tens, ones) = (bcd >> 4, bcd & 0x0F);
    (tens <= 9 && ones <= 9).then_some(tens * 10 + ones)
}

fn port_read(reg: u8) -> u8 {
    // SAFETY: `CMOS_ADDR`/`CMOS_DATA` are the fixed CMOS ports; `reg` is
    // always this module's constants or the century index
    // `acpi::rtc_century_register` already bounds below 0x80, so it never
    // sets the index port's NMI-mask bit — no value makes the RTC do
    // anything but answer a different byte.
    unsafe { cpu::outb(CMOS_ADDR, reg) };
    cpu::inb(CMOS_DATA)
}

/// The one substitution point for actuator-injected RTC faults; everything
/// downstream reads whatever this returns.
fn cmos_read(reg: u8) -> u8 {
    if crate::actuator::rtc_dead() {
        // 0xFF sets `UPDATE_IN_PROGRESS`, so a dead RTC surfaces as `Updating`.
        return 0xFF;
    }
    if crate::actuator::rtc_unstable() && reg == SECONDS {
        use core::sync::atomic::{AtomicU8, Ordering::Relaxed};
        static TICK: AtomicU8 = AtomicU8::new(0);
        // 0x01..=0x09: always valid BCD, so this stages `Unstable`, not
        // `NotADate`.
        return TICK.fetch_add(1, Relaxed) % 9 + 1;
    }
    port_read(reg)
}
