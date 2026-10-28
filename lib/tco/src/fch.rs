//! AMD's FCH watchdog timer: where it is, how it is armed, fed and stopped,
//! and what the FCH records of the last reset. Pure: the caller supplies the
//! two register windows, and a host model drives every sequence here.
//!
//! Registers are cited from AMD's PPR for Family 17h Model 18h (55570-B1
//! Rev 3.16), whose FCH chapter the public Renoir PPR (55922) does not have.
//! 58264 (Family 1Ah) and 52740 (Family 16h Models 30h-3Fh) describe the same
//! map. That Renoir's FCH matches is inferred from the generations on either
//! side of it and from its SMBus function's revision.
//!
//! **The WDT block is in the S5 power domain**, reset only by RsmRst and the
//! reset button (55570 Table 74). A CF9 reset and the watchdog's own expiry
//! leave its control and count as they were. What keeps a count the last boot
//! left harmless is `PMx000[7]`, which gates the timer as well as its window
//! (55570 and 58264, `PMx000`): firmware may clear it at reset, and where it
//! stays set [`survey`] stops the count. [`arm`] stops the timer before it
//! writes its own count. `WatchdogFired`, cleared
//! only by a cold reset, outlives warm boots the same way, so [`survey`] and
//! [`arm`] clear it wherever they can read it.

/// The function this watchdog is found by: the FCH's SMBus controller.
pub const VENDOR: u16 = 0x1022;
pub const SMBUS: u16 = 0x790b;
/// The first revision whose FCH puts the watchdog at the fixed window behind
/// `PmDecodeEn[WatchdogTmrEn]`, with its PM block in ACPI-MMIO: Family 17h
/// and later. Earlier ones keep a programmable base in PM 48h, a layout this
/// module does not drive.
pub const MIN_REVISION: u8 = 0x51;

pub fn carries(vendor: u16, device: u16, revision: u8) -> bool {
    vendor == VENDOR && device == SMBUS && revision >= MIN_REVISION
}

/// The ACPI-MMIO region, fixed on these parts (55570 PMx004 `MmioEn`, reset
/// and BIOS-init value 1; 56569 §7.2.2.1). [`AcpiMmio`] offsets count from here.
pub const ACPI_MMIO: u64 = 0xFED8_0000;
/// The bytes of it this module reads: through `MISCx0FC`.
pub const ACPI_MMIO_BYTES: u64 = 0xF00;
const PM: u16 = 0x300;

/// `PMx000 PmDecodeEn`. Bit 7, `WatchdogTmrEn`, decodes the window at [`WDT`]
/// and enables the timer (55570; 58264). Byte 3 holds `WatchDogOptions`
/// [27:26], which must be 0, and `WatchDogFreq` [25:24].
pub const PM_DECODE_EN: u16 = PM;
pub const WATCHDOG_TMR_EN: u32 = 1 << 7;
const DECODE_EN_BYTE3: u16 = PM + 3;
/// `WatchDogOptions` and `WatchDogFreq` within byte 3.
const OPTIONS_AND_FREQ: u8 = 0x0f;
/// `PMx0BE ResetCtl1`, bit 7 `RstToCpuPwrGdEn`: every FCH reset toggles
/// CpuPwrGood, a cold reset. A platform policy over CF9 and every other reset
/// alike, so it is read and reported and never written here.
pub const PM_RESET_CTL1: u16 = PM + 0xBE;
pub const RST_TO_CPU_PWR_GD_EN: u8 = 1 << 7;
/// `PMx0C0 S5ResetStat`: the source of the last reset, write-1-to-clear.
/// Hardware clears [27:16] on each new reset except the bit of that reset
/// (55570), but not every reset updates it, so it is cleared once read.
pub const PM_S5_RESET_STAT: u16 = PM + 0xC0;
/// Bit 25, `WatchdogIssueReset`.
pub const WATCHDOG_ISSUE_RESET: u32 = 1 << 25;
/// `PMx0C4` bit 2 `SelectDebug`: while set, `PMx0C0` reads as a debug register.
pub const PM_S5_RESET_STAT_SELECT: u16 = PM + 0xC4;
pub const SELECT_DEBUG: u8 = 1 << 2;
/// `PMx0C8 Misc`, bit 19 `UseCpuRst` (reset 1): when 0 a system reset asserts
/// INIT# in place of CPURST#, and an expiry would not reset the CPUs.
pub const PM_MISC: u16 = PM + 0xC8;
pub const USE_CPU_RST: u32 = 1 << 19;
/// `PMx0D5` bit 0 `AltAcpiMmioEn`: an alternate ACPI-MMIO base, which may move
/// the region this module reads at a fixed address. Refused.
pub const PM_ALT_ACPI_MMIO_EN: u16 = PM + 0xD5;
/// `MISCx0FC ClkGatingCntrl`, bit 12 `WatchDogTimerBlkClkGatingDis` (reset 1):
/// while 0 the block's clock may stop when idle, and whether it then counts is
/// undocumented (55570 §9.2.10.1). Reported.
pub const MISC_CLK_GATING: u16 = 0xEFC;
pub const WDT_BLK_CLK_GATING_DIS: u32 = 1 << 12;

/// The watchdog's own window, decoded while [`WATCHDOG_TMR_EN`] is set (55570
/// §9.2.6). Non-posted, so mapped uncached (55570 §9.1.3).
pub const WDT: u64 = 0xFEB0_0000;
/// The two registers driven here; the decode is sixteen bytes.
pub const WDT_BYTES: u64 = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Register {
    /// `WatchdogControl`.
    Control = 0x0,
    /// `WatchdogCount` [15:0]: a write sets the reload, a read is the counter.
    Count = 0x4,
}

/// The ACPI-MMIO region from [`ACPI_MMIO`]: bytes for the PM block, as its
/// registers are byte-addressed, and dwords for `PMx0C0` and `MISCx0FC`.
pub trait AcpiMmio {
    fn read_u8(&mut self, offset: u16) -> u8;
    fn write_u8(&mut self, offset: u16, value: u8);
    fn read_u32(&mut self, offset: u16) -> u32;
    fn write_u32(&mut self, offset: u16, value: u32);
}

/// The window at [`WDT`], in dwords.
pub trait Window {
    fn read(&mut self, register: Register) -> u32;
    fn write(&mut self, register: Register, value: u32);
}

/// `WatchdogControl` as read (55570 §9.2.6; 58264 §12.3.6).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Control(pub u32);

impl Control {
    /// `WatchdogRunStopB`. Setting it does not start a count interval:
    /// [`TRIGGER`](Self::TRIGGER) does, and only once running.
    pub const RUN: u32 = 1 << 0;
    /// `WatchdogFired`, write-1-to-clear: the watchdog caused the current
    /// restart. Only a cold reset clears it otherwise.
    pub const FIRED: u32 = 1 << 1;
    /// `WatchdogAction`: 0 resets the system, 1 powers it off.
    pub const POWER_OFF: u32 = 1 << 2;
    /// `WatchdogDisable`, read-only, the inverse of [`WATCHDOG_TMR_EN`].
    pub const DISABLED: u32 = 1 << 3;
    /// `watchdogactionen` on Family 1Ah, set by MP1: while 0 an expiry sets
    /// `FIRED` and does nothing else. Reserved on Family 17h.
    pub const ACTION_EN: u32 = 1 << 4;
    /// `WatchdogTrigger`, write-1-only, reads 0: restart the count from the
    /// last value written to [`Register::Count`].
    pub const TRIGGER: u32 = 1 << 7;

    /// An undecoded window reads all ones.
    pub fn decoded(self) -> bool {
        self.0 != u32::MAX
    }

    pub fn usable(self) -> bool {
        self.decoded() && self.0 & Self::DISABLED == 0
    }

    pub fn running(self) -> bool {
        self.usable() && self.0 & Self::RUN != 0
    }

    pub fn fired(self) -> bool {
        self.usable() && self.0 & Self::FIRED != 0
    }

    pub fn powers_off(self) -> bool {
        self.0 & Self::POWER_OFF != 0
    }

    /// The word that writes this one back changing nothing: `FIRED` written 0
    /// keeps it, and every other bit, `ACTION_EN` among them, is as read.
    fn kept(self) -> u32 {
        self.0 & !(Self::FIRED | Self::TRIGGER)
    }
}

/// The tick `WatchDogFreq` selects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tick {
    Us32 = 0,
    Ms10 = 1,
    Ms100 = 2,
    S1 = 3,
}

/// 100 ms: the coarsest tick at which [`crate::BOUND_MS`] is a whole count.
/// One second, the reset value, cannot express it.
pub const TICK: Tick = Tick::Ms100;
pub const TICK_MS: u64 = 100;

/// The count that is [`crate::BOUND_MS`]. One expiry resets the machine.
pub const TIMER: u16 = {
    assert!(crate::BOUND_MS.is_multiple_of(TICK_MS), "the bound is not a whole number of ticks");
    let ticks = crate::BOUND_MS / TICK_MS;
    assert!(ticks >= 1 && ticks <= u16::MAX as u64, "the bound is no 16-bit count");
    ticks as u16
};

pub const fn bound_of(count: u16) -> u64 {
    count as u64 * TICK_MS
}

/// Why [`arm`] left the machine unwatched.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// `PMx000` reads all ones: nothing decodes ACPI-MMIO.
    AcpiMmioAbsent,
    /// `PMx0D5`, with `AltAcpiMmioEn` set.
    AlternateBase(u8),
    /// `PMx000` after the write, `WatchdogTmrEn` still clear.
    DecodeStuck(u32),
    /// `PMx000` after the write, byte 3 not [`TICK`] with options 0.
    TickStuck(u32),
    /// `WatchdogControl` reads all ones with the decode on.
    WindowAbsent,
    /// `WatchdogControl` reads `WatchdogDisable`.
    Disabled(u32),
    /// `WatchdogControl` after the start, not running to a reset.
    NotStarted(u32),
}

/// Stop the timer where its window answers. [`arm`]'s refusals of a tick or
/// a start that did not take go through here, so a count left from the last
/// boot does not resume behind them; the others leave nothing to stop.
fn stop_where_decoded(fch: &mut impl Window) {
    if Control(fch.read(Register::Control)).decoded() {
        stop(fch);
    }
}

/// What [`arm`] left counting.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Armed {
    pub decode_en: u32,
    pub control: Control,
    pub count: u16,
    /// A `WatchdogFired` it found and cleared: one [`survey`] could not read,
    /// which says nothing of the last reset.
    pub cleared_fired: bool,
}

/// Enable the window, select [`TICK`], and start a count of [`TIMER`] to a
/// system reset. A refusal once the decode is on leaves no timer running.
pub fn arm(fch: &mut (impl AcpiMmio + Window)) -> Result<Armed, Refused> {
    let decode_en = fch.read_u32(PM_DECODE_EN);
    if decode_en == u32::MAX {
        return Err(Refused::AcpiMmioAbsent);
    }
    let alternate = fch.read_u8(PM_ALT_ACPI_MMIO_EN);
    if alternate & 1 != 0 {
        return Err(Refused::AlternateBase(alternate));
    }
    if decode_en & WATCHDOG_TMR_EN == 0 {
        fch.write_u8(PM_DECODE_EN, decode_en as u8 | WATCHDOG_TMR_EN as u8);
    }
    let byte3 = (decode_en >> 24) as u8;
    let wanted = byte3 & !OPTIONS_AND_FREQ | TICK as u8;
    if byte3 != wanted {
        fch.write_u8(DECODE_EN_BYTE3, wanted);
    }
    let decode_en = fch.read_u32(PM_DECODE_EN);
    if decode_en & WATCHDOG_TMR_EN == 0 {
        return Err(Refused::DecodeStuck(decode_en));
    }
    if (decode_en >> 24) as u8 & OPTIONS_AND_FREQ != TICK as u8 {
        stop_where_decoded(fch);
        return Err(Refused::TickStuck(decode_en));
    }

    let was = Control(fch.read(Register::Control));
    if !was.decoded() {
        return Err(Refused::WindowAbsent);
    }
    if !was.usable() {
        return Err(Refused::Disabled(was.0));
    }
    let stopped = was.kept() & !(Control::RUN | Control::POWER_OFF);
    fch.write(Register::Control, stopped | Control::FIRED);
    fch.write(Register::Count, u32::from(TIMER));
    // Two writes: one setting both bits does not start the count, which an
    // AMD engineer measured and the register text implies.
    fch.write(Register::Control, stopped | Control::RUN);
    fch.write(Register::Control, stopped | Control::RUN | Control::TRIGGER);

    let control = Control(fch.read(Register::Control));
    if !control.running() || control.powers_off() {
        stop_where_decoded(fch);
        return Err(Refused::NotStarted(control.0));
    }
    Ok(Armed { decode_en, control, count: fch.read(Register::Count) as u16, cleared_fired: was.fired() })
}

/// Restart the count from [`TIMER`].
pub fn feed(wdt: &mut impl Window) {
    let control = Control(wdt.read(Register::Control));
    wdt.write(Register::Control, control.kept() | Control::TRIGGER);
}

pub fn stop(wdt: &mut impl Window) {
    let control = Control(wdt.read(Register::Control));
    wdt.write(Register::Control, control.kept() & !Control::RUN);
}

/// The timer as a boot finds it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Found {
    pub control: Control,
    pub count: u16,
}

impl Found {
    pub fn read(wdt: &mut impl Window) -> Self {
        Self { control: Control(wdt.read(Register::Control)), count: wdt.read(Register::Count) as u16 }
    }

    /// Counting to a reset within [`TIMER`]: the loader's arm, which leaves
    /// no timer running when it refuses.
    pub fn armed(self) -> bool {
        self.control.running() && !self.control.powers_off() && self.count <= TIMER
    }
}

/// `PMx0C0` as [`survey`] found it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResetStat {
    /// Read, written back to clear, and read again.
    Read { stat: u32, after: u32 },
    /// `SelectDebug` is set, with `PMx0C4` as read, so it was not read.
    DebugSelected(u8),
    /// All ones, which is no status.
    Absent,
}

/// What [`survey`] read and did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Survey {
    pub decode_en: u32,
    pub reset_stat: ResetStat,
    /// `None` while [`WATCHDOG_TMR_EN`] is clear, which leaves the window
    /// undecoded.
    pub found: Option<Found>,
    /// Whether it found the timer running and stopped it.
    pub stopped: bool,
}

/// Why [`survey`] read nothing: the region it would write is not where it reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unsurveyed {
    AcpiMmioAbsent,
    AlternateBase(u8),
}

impl Survey {
    pub fn issued_reset(&self) -> bool {
        matches!(self.reset_stat, ResetStat::Read { stat, .. } if stat & WATCHDOG_ISSUE_RESET != 0)
    }

    /// `None` where the window is undecoded, which leaves it unread.
    pub fn fired(&self) -> Option<bool> {
        self.found.map(|found| found.control.fired())
    }

    /// Either record says the watchdog caused the last reset.
    pub fn watchdog_reset(&self) -> bool {
        self.issued_reset() || self.fired() == Some(true)
    }
}

/// What the loader says on a pass whose [`Survey::watchdog_reset`] holds.
pub const RESET_SAID: &str = "the FCH watchdog caused the last reset";

/// Read and clear both records of the last reset's cause, and stop a timer
/// this boot has not armed.
pub fn survey(fch: &mut (impl AcpiMmio + Window)) -> Result<Survey, Unsurveyed> {
    let decode_en = fch.read_u32(PM_DECODE_EN);
    if decode_en == u32::MAX {
        return Err(Unsurveyed::AcpiMmioAbsent);
    }
    let alternate = fch.read_u8(PM_ALT_ACPI_MMIO_EN);
    if alternate & 1 != 0 {
        return Err(Unsurveyed::AlternateBase(alternate));
    }
    let select = fch.read_u8(PM_S5_RESET_STAT_SELECT);
    let reset_stat = if select & SELECT_DEBUG != 0 {
        ResetStat::DebugSelected(select)
    } else {
        match fch.read_u32(PM_S5_RESET_STAT) {
            u32::MAX => ResetStat::Absent,
            stat => {
                if stat != 0 {
                    fch.write_u32(PM_S5_RESET_STAT, stat);
                }
                ResetStat::Read { stat, after: fch.read_u32(PM_S5_RESET_STAT) }
            }
        }
    };
    let found = (decode_en & WATCHDOG_TMR_EN != 0).then(|| Found::read(fch));
    let stopped = found.is_some_and(|found| found.control.running());
    if let Some(found) = found.filter(|found| stopped || found.control.fired()) {
        let mut word = found.control.kept();
        if stopped {
            word &= !Control::RUN;
        }
        if found.control.fired() {
            word |= Control::FIRED;
        }
        fch.write(Register::Control, word);
    }
    Ok(Survey { decode_en, reset_stat, found, stopped })
}
