//! The part, as the specification describes it — not as the driver beside it
//! expects it.
//!
//! Written from the *Intel 82574 GbE Controller Family Datasheet* (317694-018,
//! rev 2.7) and cited to it clause by clause. **The rule this file is under is
//! that it implements what the document permits, not what `lib.rs` happens to
//! do**: every latitude the datasheet gives the hardware is a [`Permits`] entry
//! here, on by default, and a driver that only works when one of them is off is
//! a driver with a bug in it. A stub that answered what the code under test
//! expected would be a finding, not a test.
//!
//! Every run takes a seed; every assertion this file makes prints it; and the
//! seed reproduces the run exactly.
//!
//! **Two parts, one model.** [`Nic::new`] is the 82574 the QEMU arm runs
//! against; §10.2.2.7 addresses its PHY as "1 = Gigabit PHY. 2 = PCIe PHY" and
//! none of that register set is modelled here, so an `MDIC` access to it is an
//! assertion by name. [`Nic::i219`] is the ThinkPad's part, and behind its
//! `MDIC` is the PHY the *Intel Ethernet Connection I219 Datasheet* (612523,
//! rev 2.02) describes — cited `§9.x` here, where the MAC's own clauses are
//! `§10.x`, `§4.x` and `§3.x`. On that part the MAC is the PCH's own, and
//! every `§8.x` here is *Intel® 500 Series Chipset Family On-Package Platform
//! Controller Hub Datasheet, Volume 2 of 2* (631120, rev 002) §8.2, which is
//! where its power and handshake registers are defined and where the rule that
//! an MDI cycle runs only while the interconnect carries one comes from.
//!
//! **Where the I219's MAC is concerned the documents stop short**, and what
//! this file models beyond them — the `LANPHYPC` power cycle, SMBus forcing,
//! the reset of the MAC and PHY together — is the properties of the part
//! `crate::wake`'s header states. What `CTRL.PHY_RST` does to the PHY's own
//! registers is in none of them, so it is left out: the model keeps them as
//! the last agent left them, and the driver has to work either way.
//!
//! What is deliberately *not* modelled: the 82574's own PHY registers, the
//! NVM's access protocol, checksum offload, VLAN insertion, RSS and the second
//! queue, flow control, and every statistic counter but the good and total
//! frames each way, which count what this model moves. The driver reaches none
//! of the rest.

use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::rc::Rc;
use std::vec::Vec;
use std::{format, vec};

use crate::phy::{advertise, control, control_1000t, custom_mode, reg};
use crate::regs::{
    self, cause, ctrl, ctrl_ext, extcnf, fwsm, lanphypc_timing, mdic, rah, rctl, rx_desc, status, tctl,
    tx_desc,
};
use crate::{phy, wake, Clock, DmaBuffers, Interrupts, Part, Registers};

/// Where the device reaches the grant. Not zero and not a small number: a
/// driver that wrote a grant *offset* into a descriptor instead of a device
/// address would still be inside a zero-based region, and this makes that
/// mistake a refusal instead of a pass.
pub const DEVICE_BASE: u64 = 0x0000_0001_0000_0000;

/// The station address the modelled NVM holds (§10.2.5.23: entry 0 is loaded
/// from the `IA` field after a reset).
pub const NVM_MAC: [u8; 6] = [0x54, 0xbf, 0x64, 0x11, 0x22, 0x33];

/// Nanoseconds the modelled clock moves on each access. A driver's deadline has
/// to be reachable, and a clock that stood still would hang the test rather
/// than fail it.
const CLOCK_STEP_NANOS: u64 = 100;

const _: () = {
    // A tick as long as the shortest bound a driver here measures makes one
    // access satisfy that bound, so a wait and no wait at all would be the same
    // thing to this model.
    assert!(CLOCK_STEP_NANOS < crate::RESET_SETTLE_NANOS);
};

/// How many reads of `CTRL` the reset stays asserted for (§10.2.2.1: the bit is
/// self-clearing, and the datasheet gives no time).
const RESET_READS: u32 = 3;

/// A latitude the datasheet gives the hardware. Each is on unless a test turns
/// it off, and each names the clause that permits it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Permits {
    /// §7.1.7.1: descriptors "accumulate and are opportunistically written out
    /// in cache line-oriented chunks", so a completion may not be visible the
    /// moment the frame arrived, and the order inside a chunk is the
    /// hardware's.
    pub batched_writeback: bool,
    /// §7.1.8: the head register "includes those descriptors completed but not
    /// yet stored in memory", so `RDH` may be ahead of what a driver can read.
    pub shadow_head: bool,
    /// §7.1.7.2: a descriptor with a null data address is written back "with
    /// the `DD` bit set in the status byte and all other bits unchanged".
    pub null_padding: bool,
    /// §7.4.5: an interrupt whose cause was already cleared. "This results in
    /// a spurious interrupt."
    pub spurious_interrupts: bool,
    /// §10.2.4.1 case 3: "Interrupt was not asserted (ICR.INT_ASSERT=0): Read
    /// has no side affect." The document's own §7.4.5 says instead that "all
    /// bits in the ICR register are cleared on a read to ICR"; both readings
    /// are of the same specification, so the model takes one or the other and
    /// a driver has to be right under either.
    pub icr_read_has_no_side_effect: bool,
    /// §3.1.3.10: the function "proceeds to issue any pending requests" after
    /// the master disable, so the status bit does not go quiet on the first
    /// read of it.
    pub master_takes_time_to_quiesce: bool,
    /// §4.5.2 gives manageability the highest priority in the MDIO arbitration
    /// and the 82574's own hardware takes the interface "while loading the
    /// extended configuration area", so a software request is not granted the
    /// first time it is made.
    pub firmware_takes_the_mdio_interface: bool,
    /// §10.2.2.7: the part sets `Ready` "at the end of the MDI transaction", so
    /// a driver may not read the register back once and believe what is in it.
    pub mdi_takes_several_reads: bool,
    /// §9.5.2.1's Power Down and Isolate are ordinary PHY state on a part whose
    /// PHY another agent drives, and nothing resets them when a claim on the
    /// function is minted.
    pub phy_starts_powered_down: bool,
    /// §9.5.2.1's Loopback and Reset are the same ordinary PHY state, and a
    /// write of that register that carries either back puts the PHY in
    /// loopback or resets it under the driver that wrote it.
    pub phy_starts_in_loopback_or_resetting: bool,
    /// §9.5.2.1: with Auto-Negotiation Enable clear "the link configuration is
    /// determined manually", which is what an agent that forced this link's
    /// speed left behind and nothing resets when a claim is minted.
    pub phy_starts_with_autonegotiation_disabled: bool,
    /// §5.2: "the integrated LAN controller configures the LCD registers", and
    /// §6.1.5's Auto-Connect Battery Saver has the last driver "negotiate to
    /// the lowest connection speed supported by the link partner (usually
    /// 10 Mb/s) when the power cable is unplugged" — so §9.5.2.5's advertisement
    /// out of a claim is what the agent before this one left in it, not the
    /// power-on default.
    pub phy_advertises_what_the_last_agent_left: bool,
    /// §4.5.2 arbitrates three bits of `EXTCNF_CTRL` and no more, so what
    /// stands in the rest of that register is another agent's and a part comes
    /// up with some of it set.
    pub extcnf_carries_firmware_fields: bool,
    /// §4.5.2 has the software ownership bit read 0b until the arbitration
    /// grants it, so a requester can tell a grant from its own request by
    /// reading that one bit; read as a plain mutex, the same bit reads back set
    /// for whichever agent set it, granted or not. Both are readings of this
    /// one interface, so the model takes one or the other and a driver has to
    /// be right under either.
    pub mdio_flag_is_a_plain_mutex: bool,
    /// §8.2.1's `PHYPDN` is a writable bit the document describes only in its
    /// cleared state, so a part whose firmware left it set is one a driver has
    /// to find and clear — and I219 §2.2's Table 2-1 puts the interconnect in
    /// Electrical Idle while the PHY is down, so the MDI cycle does not run.
    pub mac_comes_up_holding_the_phy_down: bool,
    /// §8.2.2's `PHYPDEN` is loaded from the NVM (I219 §10.3.1.9's "PHY PD
    /// Ena" word, "loaded to the PHY Power Down Enable bit in the Extended
    /// Device Control (CTRL_EXT) register"), so a part comes up with it set.
    pub low_power_entry_comes_up_enabled: bool,
    /// §8.2.3's `Wait`: "Set to 1 by the Gigabit Ethernet Controller to
    /// indicate that a PCI Express* to SMBus transition is taking place", and
    /// auto-cleared afterwards — so a driver reaching the register may find one
    /// in flight.
    pub the_interconnect_is_in_transition: bool,
    /// §8.2.9's `FWVAL`: "1 = Firmware is ready. 0 = Firmware is not ready."
    /// The document attaches nothing to the cleared case, so a part that
    /// reports it is one a driver reads and goes on from.
    pub firmware_reports_itself_ready: bool,
    /// §4.5.2's manageability request comes whenever the engine likes — the
    /// moment after another agent has read the interface free included — and
    /// "the priority order is manageability, software and then hardware", so a
    /// software request registered right after that read is answered second.
    pub firmware_requests_after_a_free_read: bool,
    /// I219 §6.4's Ultra Low Power and §12.1.4's SMBus mode are states the
    /// agent before this driver leaves a PHY in, and in neither does the PHY
    /// answer an MDI cycle the MAC sends over PCIe — so a claim is minted on a
    /// PHY out of `MDIC`'s reach.
    pub phy_starts_out_of_reach: bool,
    /// I219 §6.4's ULP configuration register has a field "Reset to SMBus by
    /// default (on power on or on ULP exit)" that the agent before this driver
    /// may have set, so a power-cycled PHY may come back on SMBus.
    pub phy_comes_back_on_smbus: bool,
    /// `CTRL.RST` alone resets the MAC's end of the interconnect and not the
    /// PHY's (`crate::wake`'s header), so a reset of the MAC alone may leave
    /// the two ends of it out of step.
    pub mac_reset_alone_loses_the_phy: bool,
    /// `FWSM` bit 6: the firmware allows the host to reset the PHY — what the
    /// T14's firmware reports.
    pub firmware_allows_a_phy_reset: bool,
    /// I219 §7.4: the operating system before this driver may have armed the
    /// PHY for host wake-up — `MACPD_enable` and then `Host_WU_Active` in
    /// §9.5.3.2's Port General Configuration — and §9.5.3.2 has that bit
    /// "reset by power on reset only", so it survives every reset but a power
    /// cycle.
    pub phy_wake_left_armed: bool,
}

impl Default for Permits {
    fn default() -> Self {
        Self {
            batched_writeback: true,
            shadow_head: true,
            null_padding: true,
            spurious_interrupts: true,
            icr_read_has_no_side_effect: true,
            master_takes_time_to_quiesce: true,
            firmware_takes_the_mdio_interface: true,
            mdi_takes_several_reads: true,
            phy_starts_powered_down: true,
            phy_starts_in_loopback_or_resetting: true,
            phy_starts_with_autonegotiation_disabled: true,
            phy_advertises_what_the_last_agent_left: true,
            extcnf_carries_firmware_fields: true,
            mdio_flag_is_a_plain_mutex: true,
            firmware_requests_after_a_free_read: true,
            mac_comes_up_holding_the_phy_down: true,
            low_power_entry_comes_up_enabled: true,
            the_interconnect_is_in_transition: true,
            firmware_reports_itself_ready: true,
            phy_starts_out_of_reach: true,
            phy_comes_back_on_smbus: true,
            mac_reset_alone_loses_the_phy: true,
            firmware_allows_a_phy_reset: true,
            phy_wake_left_armed: true,
        }
    }
}

impl Permits {
    /// The same latitudes with the PHY's four taken away: those four are only
    /// what the agent before this driver left in the register file, and
    /// §9.5.2.1's reset is the event that takes them away.
    fn after_a_phy_reset(&self) -> Self {
        Self {
            phy_starts_powered_down: false,
            phy_starts_in_loopback_or_resetting: false,
            phy_starts_with_autonegotiation_disabled: false,
            phy_advertises_what_the_last_agent_left: false,
            ..*self
        }
    }
}

/// §9.3: registers 0 to 15 "are identical in all the pages and are the IEEE
/// defined registers", and everything above them is the vendor's and therefore
/// the page's. An access to one of those without a page selected first reaches
/// whichever page was left behind.
const FIRST_PAGED_REGISTER: u8 = 16;

/// One register's §9.1 contract: the fields a driver does not compose, and what
/// §9.5's own table says each of them comes up as.
struct Carried {
    mask: u16,
    default: u16,
}

/// §9.1's tables, register by register: "other fields in the same 16-bit
/// register must be loaded with their default values".
mod carried {
    use super::Carried;
    use crate::phy::{control_1000t, custom_mode};

    /// §9.5.2.1: Speed Selection (MSB, bit 6) and Duplex Mode (bit 8) come up
    /// 1b, Collision Test (bit 7) and Speed Select (LSB, bit 13) come up 0b,
    /// and bits 5:0 are "Reserved. Always set to 0x0".
    pub const CONTROL: Carried = Carried { mask: 0x21FF, default: (1 << 6) | (1 << 8) };

    /// §9.5.2.5's whole default is `0x01E1` — the selector and the four
    /// abilities at bits 8:0 and nothing above them.
    pub const ADVERTISE: Carried = Carried { mask: !0x01FF, default: 0 };

    /// §9.5.2.10's table gives every other field of this register a default of
    /// `0b` — bits 7:0 "Reserved. Set these bits to 0x00", Advertise
    /// 1000BASE-T Half-Duplex (which the same note says this PHY does not
    /// support), Port Type, both Master/Slave fields and Test Mode.
    pub const CONTROL_1000T: Carried = Carried { mask: !control_1000t::FULL, default: 0 };

    /// §9.5.3.1's two reserved fields either side of bit 10: 0x180 at bits 9:0
    /// and 0x04 at bits 15:11.
    pub const CUSTOM_MODE: Carried =
        Carried { mask: !custom_mode::REDUCED_MDIO_FREQUENCY, default: 0x2180 };
}

/// §9.5.3.2's defaults: reserved bit 5 `1b` and Tx Gate Wait IFS `00111b` at
/// bits 15:11, every other field `0b`.
const PORT_GENERAL_DEFAULT: u16 = (1 << 5) | (0b00111 << 11);

/// How many `STATUS` reads §3.1.3.10's master enable stays set for.
const MASTER_QUIESCE_READS: u32 = 3;

/// How many reads of `EXTCNF_CTRL` §4.5.2's manageability agent holds the
/// interface across before the software request registered under it is granted.
const MDIO_FIRMWARE_REQUESTS: u32 = 2;

/// §9.5.2.5's whole default: Selector Field `00001b` and the four 10/100
/// abilities at bits 8:5, every other field `0b`.
const ADVERTISE_DEFAULT: u16 = 0x01E1;

/// §6.1.5's battery saver left the link at "the lowest connection speed
/// supported by the link partner": §9.5.2.5's selector and 10BASE-T alone.
const ADVERTISE_AFTER_BATTERY_SAVER: u16 = 0x0061;

/// How many `MDIC` reads a transaction takes before §10.2.2.7's `Ready` is set.
const MDI_READS: u32 = 2;

/// How many `MDIC` reads §8.2.3's `Wait` stands across before the interconnect
/// transition it reports has "occurred".
const MDI_WAIT_READS: u32 = 3;

/// §10.2.2.1's two reserved `CTRL` bits, documented as "Set to 1b" (bit 3) and
/// "must be set to 1b" (bit 20, `ADVD3WUC`).
const CTRL_RESERVED_SET: u32 = (1 << 3) | (1 << 20);

/// What another agent left in `EXTCNF_CTRL` outside §4.5.2's three ownership
/// bits. The value is arbitrary and its only property is that this driver never
/// wrote it.
const EXTCNF_FIRMWARE_FIELDS: u32 = 1 << 13;

/// Where the PHY stands with respect to the MAC's end of their interconnect.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lcd {
    /// On PCIe and in step with the MAC: an MDI cycle over PCIe ends.
    InStep,
    /// On SMBus: a cycle ends only from a MAC forced onto SMBus too.
    Smbus,
    /// Its end of the interconnect out of step with the MAC's, which a reset of
    /// both together or a power cycle puts right.
    OutOfStep,
    /// Powered down, or in Ultra Low Power: only a power cycle reaches it.
    Off,
}

/// How many readings of `STATUS` after a full reset its PHY-configured bit
/// stays clear for.
const PHY_CONFIGURED_READS: u32 = 3;

/// How many readings of `CTRL_EXT` after a power cycle its cycle-done bit
/// stays clear for.
const CYCLE_DONE_READS: u32 = 1;

/// The PHY the *Intel Ethernet Connection I219 Datasheet* describes, as much of
/// it as reaches this driver.
///
/// **The whole of the link is here**: [`Model::refresh_phy_link`] raises one
/// only when every clause of §9 the bring-up is under has been satisfied, and
/// [`Model::phy_unconfigured`] names the first that has not. A driver that
/// leaves out one register write gets a part that negotiates nothing, which is
/// what the silicon does.
struct PhyModel {
    /// §9.3's page register at PHY address 01, and `None` until one is
    /// selected: a paged register touched before then reaches whichever page
    /// the last agent left behind.
    page: Option<u16>,
    /// §9.5.3.1's Custom Mode Control, PHY address 01 page 769 register 16.
    custom_mode: u16,
    /// §9.5.3.2's Port General Configuration, register 17 of the same page,
    /// which only a power-on reset returns to its defaults.
    port_general: u16,
    /// PHY address 02's registers 0 through 15, which §9.3 says "are identical
    /// in all the pages and are the IEEE defined registers".
    file: [u16; 16],
    /// What §9.5.2.5's and §9.5.2.10's ability registers held when negotiation
    /// last restarted — and therefore what is on the wire, since §9.5.2.5 says
    /// the advertisement "is not updated following auto-negotiation".
    negotiated_over: Option<(u16, u16)>,
    up: bool,
    /// Whether a restart of auto-negotiation is still running. §9.5.2.2's
    /// Auto-Negotiation Complete is clear and no link is reported while it is,
    /// and on 1000BASE-T that interval is seconds — far past anything a
    /// bring-up reads inside itself. [`Nic::negotiation_settles`] is the wire
    /// event that ends it.
    negotiating: bool,
    /// Reads of `MDIC` left before the transaction in flight reports `Ready`.
    mdi_reads: u32,
    /// Reads of `MDIC` left before §8.2.3's `Wait` auto-clears, which the
    /// document says it does "after the transition has occurred".
    wait_reads: u32,
    /// Whether that transition never occurs. A fault injector and not a
    /// clause: it is how a test reaches [`crate::phy::PhyRefusal::MdiWaiting`].
    wait_sticks: bool,
    /// Whether §4.5.2's software request is registered — "a request for
    /// ownership is registered by writing a 1b into the respective bit", which
    /// stands until the agent writes a 0b back whether or not it was granted.
    sw_requested: bool,
    /// Reads of `EXTCNF_CTRL` left before §4.5.2's arbitration grants the
    /// registered software request, and whether it ever does.
    firmware_requests: u32,
    mdio_sticks: bool,
    /// A PHY address nothing drives, whose reads answer ones. Not an injected
    /// fault but §9.3's other reading: the document places the IEEE registers
    /// at both of its two addresses and a part answers them at one.
    deaf: Option<u8>,
    /// Whether an MDI read of this register answers §10.2.2.7's `Error`.
    failing: Option<(u8, u8)>,
    /// Whether §10.2.2.7's `Ready` never comes back at all. A fault injector
    /// and not a clause: it is how a test reaches
    /// [`crate::phy::PhyRefusal::MdiUnready`].
    never_ready: bool,
    /// Whether another agent is holding §4.5.2's *software* flag — the state a
    /// claim on this function inherits from firmware that drives the same PHY.
    flag_held_by_another_agent: bool,
    /// Whether §4.5.2's third agent — the part's own hardware, whose bit 6 is
    /// read-only to software — is holding the interface. §4.5.2 has it take one
    /// "while loading the extended configuration area", and nothing a driver
    /// writes ends that.
    hardware_holds: bool,
    /// §4.5.2's three ownership bits as this part answered them, one entry per
    /// change: what an agent watching the interface would have seen, so a test
    /// about *which* reading a refusal carries can say the reading moved.
    ownership_readings: Vec<u32>,
    /// How many times the engine registered its request right after an agent
    /// read the interface free, so a test about that race can see it happened.
    engine_cut_ins: u32,
    /// The modelled clock at which an engine that holds the interface writes
    /// its bit back to 0b — §4.5.2: "once the access completes, the
    /// controlling agent must write a 0b to its ownership bit" — so a test
    /// can place the grant on either side of a bound.
    engine_lets_go_at: Option<u64>,
    /// Reads of `EXTCNF_CTRL` left that answer ones. **A fault injector and
    /// not a clause**: a window that stops decoding for a moment, which is how
    /// a test reaches the arbitration's retry of its own release.
    extcnf_ones: u32,
    /// How many reads answer ones once this driver's request is written: the
    /// same fault, placed inside the grant wait.
    extcnf_ones_after_request: Option<u32>,
    /// Every write this driver made to `EXTCNF_CTRL`, so a test about a write
    /// that must not happen can say it did not.
    extcnf_writes: u32,
    /// Whether moving the MAC onto SMBus starts a transition that never ends.
    /// A fault injector: it is how a test reaches a ladder that stops on a
    /// refusal with the MAC on SMBus.
    transition_sticks_on_smbus: bool,
}

impl PhyModel {
    /// The register file out of power-on, every default §9.5.2's tables print.
    fn new(permits: &Permits) -> Self {
        let mut file = [0u16; 16];
        // §9.5.2.1: Speed Selection (MSB), Duplex Mode and Auto-Negotiation
        // Enable are the three bits that come up set.
        file[reg::CONTROL as usize] = (1 << 6) | (1 << 8) | control::AUTONEG_ENABLE;
        if permits.phy_starts_powered_down {
            file[reg::CONTROL as usize] |= control::POWER_DOWN | control::ISOLATE;
        }
        if permits.phy_starts_in_loopback_or_resetting {
            file[reg::CONTROL as usize] |= control::LOOPBACK | control::RESET;
        }
        if permits.phy_starts_with_autonegotiation_disabled {
            file[reg::CONTROL as usize] &= !control::AUTONEG_ENABLE;
        }
        // §9.5.2.3 and §9.5.2.4: Intel's OUI, model 0xA, revision 0x1.
        file[reg::IDENTIFIER_HIGH as usize] = phy::IDENTIFIER_HIGH_INTEL;
        file[reg::IDENTIFIER_LOW as usize] = 0x00A1;
        file[reg::ADVERTISE as usize] = if permits.phy_advertises_what_the_last_agent_left {
            ADVERTISE_AFTER_BATTERY_SAVER
        } else {
            ADVERTISE_DEFAULT
        };
        Self {
            page: None,
            custom_mode: carried::CUSTOM_MODE.default,
            port_general: if permits.phy_wake_left_armed {
                PORT_GENERAL_DEFAULT
                    | phy::port_general::MACPD_ENABLE
                    | phy::port_general::HOST_WAKE_UP_ACTIVE
            } else {
                PORT_GENERAL_DEFAULT
            },
            file,
            wait_reads: if permits.the_interconnect_is_in_transition { MDI_WAIT_READS } else { 0 },
            wait_sticks: false,
            negotiated_over: None,
            up: false,
            negotiating: false,
            mdi_reads: 0,
            sw_requested: false,
            firmware_requests: if permits.firmware_takes_the_mdio_interface {
                MDIO_FIRMWARE_REQUESTS
            } else {
                0
            },
            mdio_sticks: false,
            deaf: None,
            failing: None,
            never_ready: false,
            flag_held_by_another_agent: false,
            hardware_holds: false,
            ownership_readings: Vec::new(),
            engine_cut_ins: 0,
            engine_lets_go_at: None,
            extcnf_ones: 0,
            extcnf_ones_after_request: None,
            extcnf_writes: 0,
            transition_sticks_on_smbus: false,
        }
    }
}

/// The register file plus everything behind it.
struct Model {
    seed: u64,
    rng: u64,
    part: Part,
    permits: Permits,
    /// The PHY, on the part that has one behind `MDIC`.
    phy: PhyModel,
    /// What a read of `MDIC` answers once the transaction in flight is over.
    mdi_answer: u32,
    /// Reads of `STATUS` left before §3.1.3.10's master enable goes quiet, and
    /// whether it ever does.
    master_reads: u32,
    master_sticks: bool,
    /// The modelled clock when `CTRL.RST` was last written, so §10.2.2.1's
    /// settling time and §9.2's delay before an MDIO access can be held to.
    reset_at: u64,
    /// What the partner on the wire advertises: §9.5.2.5's register and
    /// §9.5.2.10's, the same pair this part offers. A partner that offers less
    /// than everything is what makes this driver's own advertisement decide
    /// the speed at all.
    partner: (u16, u16),
    file: Vec<u32>,
    memory: Vec<u8>,
    nanos: u64,
    /// Reads of `CTRL` left before `RST` clears itself.
    reset_reads: u32,
    /// Whether the modelled NVM answers, and therefore whether `RAH0.AV` is
    /// set after a reset (§10.2.5.23).
    has_nvm: bool,
    /// One register this part does not take a write to. **A fault injector and
    /// not a modelled behaviour**: it is how a test reaches
    /// [`crate::Refusal::NotAccepted`].
    refusing: Option<usize>,
    /// Whether `CTRL.RST` never clears itself, against §10.2.2.1's "this bit is
    /// self-clearing". A fault injector too: how a test reaches
    /// [`crate::Refusal::ResetUnfinished`].
    reset_sticks: bool,
    /// Whether the claim has stopped answering its interrupt record at all.
    claim_gone: bool,
    link_up: bool,
    /// `STATUS.SPEED`'s encoding: `10b` is 1000 Mb/s.
    speed_code: u32,
    /// Frames the wire has delivered and the device has not yet placed.
    inbound: VecDeque<Vec<u8>>,
    /// Frames the device has put on the wire.
    pub sent: Vec<Vec<u8>>,
    /// The device's own receive head: descriptors it has taken and filled.
    rx_head: usize,
    /// Receive descriptors the device has taken and not written back: the
    /// index, the status word it will report, and the bytes it will leave in
    /// the buffer. **The frame travels with the write-back**, because that is
    /// what `DD` promises — §7.1.3.3: "DD indicates whether hardware is done
    /// with the descriptor. When the DD bit is set along with EOP, the received
    /// packet is completely in main memory." Before it, what is in the buffer
    /// is the device's business, and this model puts a pattern there to say so.
    rx_holding: Vec<(usize, u64, Vec<u8>)>,
    tx_head: usize,
    tx_holding: Vec<usize>,
    /// Messages the function has sent that the claim has not read.
    messages: u32,
    /// One offset in the window that nothing decodes, which answers ones.
    not_decoding: Option<usize>,
    /// Every register offset the driver has written, so a test about what a
    /// path does *not* write can say so.
    written: BTreeSet<usize>,
    /// Whether the device does its work when a tail register is written.
    ///
    /// **Nothing in the datasheet says *when* the hardware acts** — a fetch
    /// happens "as soon as any descriptors are made available" (§7.2.4.1) and
    /// a write-back "opportunistically" (§7.1.7.1) — so a test may hold it and
    /// watch a driver meet a device that has not caught up.
    held: bool,
    /// The modelled clock at every access the driver made to `EXTCNF_CTRL`.
    ///
    /// **Recorded and never judged here.** What the gaps between them have to
    /// be is a bench fact about one machine and no clause of any document, so
    /// the model notes when this driver reached the arbitration and a test
    /// holds the gaps to [`crate::phy::ARBITRATION_PACE_NANOS`].
    arbitration_at: Vec<u64>,
    /// Where the PHY stands with respect to the MAC.
    lcd: Lcd,
    /// When `LANPHYPC` was last driven low, and not yet let go.
    lanphypc_low_at: Option<u64>,
    /// Readings of `CTRL_EXT` left before the cycle-done bit shows.
    cycle_done_reads: u32,
    /// When the MAC was last forced onto SMBus.
    smbus_forced_at: Option<u64>,
    /// No access is taken before this: the 20 ms after a full reset.
    quiet_until: Option<u64>,
    /// Readings of `STATUS` left before its bit 9 shows, and `None` where
    /// no reset that reached the PHY is in progress.
    phy_configured_reads: Option<u32>,
    /// When the PHY was last reset or power-cycled, and `None` for a PHY that
    /// came up long before the claim was minted — which §9.2's 10 ms before an
    /// MDIO access is measured from.
    lcd_reset_at: Option<u64>,
    power_cycles: u32,
    phy_resets: u32,
}

/// Where one PHY register address lands in [`PhyModel`].
enum PhyPlace {
    /// One of §9.3's "IEEE defined registers", 0 through 15.
    Ieee(u8),
    /// §9.3's page register at PHY address 01, register 31.
    Page,
    /// §9.5.3.1's Custom Mode Control, which is page 769's.
    CustomMode,
    /// §9.5.3.4's SMBus Control, page 769's register 23.
    SmbusControl,
    /// §9.5.3.2's Port General Configuration, page 769's register 17.
    PortGeneral,
    /// One of the thirty PHY addresses §9.3 places nothing at. §10.2.2.7's
    /// `PHYADD` field is five bits and this part answers two of them, so a
    /// transaction to any other ends with nothing driving the bus.
    Undriven,
}

/// A tiny seeded generator. Not cryptography and not a distribution: a way to
/// make "the hardware chose" reproducible from one printed number.
fn next(rng: &mut u64) -> u64 {
    let mut x = *rng;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *rng = x;
    x
}

impl Model {
    fn new(seed: u64, part: Part, permits: Permits) -> Self {
        let mut model = Self {
            seed,
            rng: seed | 1,
            part,
            phy: PhyModel::new(&permits),
            mdi_answer: 0,
            master_reads: 0,
            master_sticks: false,
            reset_at: 0,
            partner: (ADVERTISE_DEFAULT, control_1000t::FULL),
            permits,
            file: vec![0; regs::REGISTER_BYTES / 4],
            memory: vec![0; crate::GRANT_BYTES as usize],
            nanos: 0,
            reset_reads: 0,
            has_nvm: true,
            refusing: None,
            reset_sticks: false,
            claim_gone: false,
            link_up: false,
            speed_code: 0b10,
            inbound: VecDeque::new(),
            sent: Vec::new(),
            rx_head: 0,
            rx_holding: Vec::new(),
            tx_head: 0,
            tx_holding: Vec::new(),
            messages: 0,
            not_decoding: None,
            written: BTreeSet::new(),
            held: false,
            arbitration_at: Vec::new(),
            lcd: if permits.phy_starts_out_of_reach { Lcd::Off } else { Lcd::InStep },
            lanphypc_low_at: None,
            cycle_done_reads: 0,
            smbus_forced_at: None,
            quiet_until: None,
            phy_configured_reads: None,
            lcd_reset_at: None,
            power_cycles: 0,
            phy_resets: 0,
        };
        model.power_on();
        model
    }

    /// What the register file holds out of reset: the fields the documents give
    /// a value, which are the fields a driver has to carry.
    fn power_on(&mut self) {
        for word in self.file.iter_mut() {
            *word = 0;
        }
        self.set(regs::CTRL, CTRL_RESERVED_SET);
        if self.permits.extcnf_carries_firmware_fields {
            self.set(regs::EXTCNF_CTRL, EXTCNF_FIRMWARE_FIELDS);
        }
        // §8.2's own defaults, which exist only on the part whose MAC that
        // document describes.
        if self.part == Part::I219 {
            // §8.2.5: "Default: Ch".
            self.set(regs::PHY_CTRL, crate::power::PHY_CTRL_RESET);
            if self.permits.firmware_reports_itself_ready {
                self.set(regs::FWSM, fwsm::FIRMWARE_VALID);
            }
            if self.permits.firmware_allows_a_phy_reset {
                self.set(regs::FWSM, self.get(regs::FWSM) | fwsm::PHY_RESET_ALLOWED);
            }
            if self.permits.mac_comes_up_holding_the_phy_down {
                self.set(regs::CTRL, self.get(regs::CTRL) | ctrl::PHY_POWER_DOWN);
            }
            if self.permits.low_power_entry_comes_up_enabled {
                self.set(regs::CTRL_EXT, ctrl_ext::PHY_POWER_DOWN_ENABLE);
            }
            self.phy.wait_reads = if self.phy.wait_sticks {
                u32::MAX
            } else if self.permits.the_interconnect_is_in_transition {
                MDI_WAIT_READS
            } else {
                0
            };
        }
        self.rx_head = 0;
        self.tx_head = 0;
        self.master_reads = 0;
        self.rx_holding.clear();
        self.tx_holding.clear();
        // §4.5.2: the software and hardware ownership bits "are cleared on
        // reset", and the part takes the interface again "while loading the
        // extended configuration area" — which is what a reset makes it do.
        if self.permits.firmware_takes_the_mdio_interface {
            self.phy.firmware_requests = MDIO_FIRMWARE_REQUESTS;
        }
        self.load_station_address();
        self.refresh_status();
    }

    fn load_station_address(&mut self) {
        if !self.has_nvm {
            self.set(regs::RAL0, 0);
            self.set(regs::RAH0, 0);
            return;
        }
        let m = NVM_MAC;
        let low = u32::from_le_bytes([m[0], m[1], m[2], m[3]]);
        let high = rah::AV | u16::from_le_bytes([m[4], m[5]]) as u32;
        self.set(regs::RAL0, low);
        self.set(regs::RAH0, high);
    }

    fn refresh_status(&mut self) {
        // §3.1.3.10: the bit is "Cleared by the 82574 when the PCIe Master
        // Disable bit is set and no master requests are pending by the relevant
        // function, set otherwise".
        let mut value = if self.get(regs::CTRL) & ctrl::GIO_MASTER_DISABLE == 0
            || self.master_reads > 0
        {
            status::GIO_MASTER_ENABLE
        } else {
            0
        };
        // §4.6.3.2: `LU` "Reflects link indication (LINK) from the PHY
        // qualified with CTRL.SLU", and on the part with a PHY behind `MDIC`
        // that indication is the one §9.5.2.2 reports.
        let link = match self.part {
            Part::I219 => self.phy.up,
            Part::E82574 => self.link_up,
        };
        if link && self.get(regs::CTRL) & ctrl::SLU != 0 {
            let (speed_code, full_duplex) = match self.part {
                Part::I219 => self.resolution().expect("a link the PHY raised has a resolution"),
                Part::E82574 => (self.speed_code, true),
            };
            value |= status::LU;
            if full_duplex {
                value |= status::FD;
            }
            value |= (speed_code & status::SPEED_MASK) << status::SPEED_SHIFT;
        }
        if self.phy_configured_reads == Some(0) {
            value |= status::PHY_CONFIGURED;
        }
        self.set(regs::STATUS, value);
    }

    /// What §9.5.2.5's and §9.5.2.10's advertisement, taken against the
    /// partner's, resolves to: `STATUS.SPEED`'s encoding and whether the
    /// resolution is full duplex. `None` is no ability in common, which is a
    /// cable that carries no link.
    ///
    /// The highest common ability wins, which is what auto-negotiation is; the
    /// 1000BASE-T ability is its own register because §9.5.2.10 puts it there.
    fn resolution(&self) -> Option<(u32, bool)> {
        let (mine, mine_1000t) = self.phy.negotiated_over?;
        if mine & advertise::SELECTOR_802_3 == 0 {
            return None;
        }
        let (partner, partner_1000t) = self.partner;
        if mine_1000t & partner_1000t & control_1000t::FULL != 0 {
            return Some((0b10, true));
        }
        let common = mine & partner;
        for (ability, speed, full) in [
            (advertise::FULL_100, 0b01, true),
            (advertise::HALF_100, 0b01, false),
            (advertise::FULL_10, 0b00, true),
            (advertise::HALF_10, 0b00, false),
        ] {
            if common & ability != 0 {
                return Some((speed, full));
            }
        }
        None
    }

    /// §9 clause by clause: the first thing the PHY needs and has not been
    /// given, or `None` when it can raise a link.
    fn phy_unconfigured(&self) -> Option<&'static str> {
        if self.phy.custom_mode & custom_mode::REDUCED_MDIO_FREQUENCY == 0 {
            return Some("§9.2's bit 10 of page 769 register 16 was never set");
        }
        let control = self.phy.file[reg::CONTROL as usize];
        if control & control::POWER_DOWN != 0 {
            return Some("§9.5.2.1's Power Down was left set");
        }
        if control & control::ISOLATE != 0 {
            return Some("§9.5.2.1's Isolate was left set, so the PHY is off the MII");
        }
        if control & control::LOOPBACK != 0 {
            return Some("§9.5.2.1's Loopback was left set");
        }
        if control & control::AUTONEG_ENABLE == 0 {
            return Some("§9.5.2.1's Auto-Negotiation Enable was left clear");
        }
        let abilities =
            (self.phy.file[reg::ADVERTISE as usize], self.phy.file[reg::CONTROL_1000T as usize]);
        if self.phy.negotiated_over != Some(abilities) {
            return Some(
                "§9.5.2.1's Restart Auto-Negotiation was never written after the abilities \
                 last changed, so what is on the wire is not what the registers hold",
            );
        }
        if self.phy.negotiating {
            return Some("the restart of auto-negotiation has not resolved yet");
        }
        if self.resolution().is_none() {
            return Some(
                "§9.5.2.5's and §9.5.2.10's advertisement has no ability in common with the \
                 partner on the wire",
            );
        }
        None
    }

    /// Take the PHY's link to what its registers and the wire together make it.
    fn refresh_phy_link(&mut self) {
        let up = self.link_up && self.phy_unconfigured().is_none();
        if up == self.phy.up {
            return;
        }
        self.phy.up = up;
        self.refresh_status();
        self.raise(cause::LSC);
    }

    fn get(&self, reg: usize) -> u32 {
        self.file[reg / 4]
    }

    fn set(&mut self, reg: usize, value: u32) {
        self.file[reg / 4] = value;
    }

    /// One 32-bit read of the register file, with the side effects the
    /// datasheet gives each register.
    fn read(&mut self, reg: usize) -> u32 {
        assert!(
            reg.is_multiple_of(4) && reg + 4 <= regs::REGISTER_BYTES,
            "seed {}: a {reg:#x} register read is outside the file",
            self.seed
        );
        self.keep_quiet(reg);
        // Nothing decodes this offset, so the bus answers ones — which is a
        // value and not a register's value.
        if self.not_decoding == Some(reg) {
            return u32::MAX;
        }
        match reg {
            regs::CTRL_EXT => {
                let value = self.get(regs::CTRL_EXT);
                if self.cycle_done_reads > 0 {
                    self.cycle_done_reads -= 1;
                    if self.cycle_done_reads == 0 {
                        self.set(regs::CTRL_EXT, value | ctrl_ext::LANPHYPC_CYCLE_DONE);
                    }
                }
                value
            }
            regs::CTRL => {
                if self.reset_reads > 0 {
                    assert!(
                        self.nanos.saturating_sub(self.reset_at) >= crate::RESET_SETTLE_NANOS,
                        "seed {}: the driver read CTRL {} ns after writing the reset, and \
                         §10.2.2.1 says \"designers must wait approximately 1 us after \
                         resetting before attempting to check to see if the bit has cleared\"",
                        self.seed,
                        self.nanos.saturating_sub(self.reset_at)
                    );
                }
                if self.reset_reads > 0 && !self.reset_sticks {
                    self.reset_reads -= 1;
                    if self.reset_reads == 0 {
                        // §10.2.2.1: "This bit is self-clearing." The reset
                        // itself has already happened; this is the bit going
                        // away.
                        let held = self.get(regs::CTRL) & !ctrl::RST;
                        self.set(regs::CTRL, held);
                    }
                }
                self.get(regs::CTRL)
            }
            regs::STATUS => {
                if self.master_reads > 0 {
                    self.master_reads -= 1;
                }
                if let Some(left) = self.phy_configured_reads.as_mut() {
                    *left = left.saturating_sub(1);
                }
                self.refresh_status();
                self.get(regs::STATUS)
            }
            regs::MDIC => {
                self.not_modelled_on_the_82574("MDIC");
                // §8.2.3: `Wait` is set by the controller for a PCIe-to-SMBus
                // transition and "is auto cleared by hardware after the
                // transition has occurred".
                if self.phy.wait_reads > 0 {
                    if !self.phy.wait_sticks {
                        self.phy.wait_reads -= 1;
                    }
                    return self.mdi_answer | mdic::WAIT;
                }
                if self.phy.mdi_reads > 0 {
                    self.phy.mdi_reads -= 1;
                    // §10.2.2.7: `Ready` is set "at the end of the MDI
                    // transaction" and `Error` "when it fails to complete an
                    // MDI read", so until the transaction ends the register
                    // carries neither and reads the command the driver put in.
                    return self.mdi_answer
                        & !mdic::READY
                        & !mdic::ERROR
                        & !mdic::DATA_MASK;
                }
                self.mdi_answer
            }
            regs::EXTCNF_CTRL => {
                self.not_modelled_on_the_82574("EXTCNF_CTRL");
                self.arbitration_at.push(self.nanos);
                if self.phy.extcnf_ones > 0 {
                    self.phy.extcnf_ones -= 1;
                    return u32::MAX;
                }
                // §4.5.2's arbitration runs on its own: the request registered
                // by one write is granted when the agent ahead of it lets go,
                // and the requester learns that by reading the bit back.
                if self.phy.firmware_requests > 0 {
                    self.phy.firmware_requests -= 1;
                }
                if self.phy.engine_lets_go_at.is_some_and(|at| self.nanos >= at) {
                    self.phy.engine_lets_go_at = None;
                    self.phy.mdio_sticks = false;
                }
                self.refresh_ownership();
                let answer = self.get(regs::EXTCNF_CTRL);
                // The manageability request nothing schedules: the one that
                // lands between an agent reading the interface free and
                // whatever that agent does about it.
                if answer & extcnf::OWNERSHIP == 0
                    && self.permits.firmware_requests_after_a_free_read
                {
                    self.phy.firmware_requests = MDIO_FIRMWARE_REQUESTS;
                    self.phy.engine_cut_ins += 1;
                }
                answer
            }
            regs::ICR => {
                let value = self.get(regs::ICR);
                let masked_all = self.get(regs::IMS) == 0;
                let asserted = value & cause::INT_ASSERTED != 0;
                // §10.2.4.1's three cases. Case 1 clears unconditionally; case
                // 3 does nothing at all — and whether case 3 or §7.4.5's
                // blanket read-to-clear applies is what `Permits` chooses.
                if masked_all || asserted || !self.permits.icr_read_has_no_side_effect {
                    self.set(regs::ICR, 0);
                }
                value
            }
            // §10.2.4.6 and §10.2.4.4: write-only, and a read of one answers
            // nothing.
            regs::IMC | regs::ICS => 0,
            // The statistics the driver reads: each read takes the count and
            // clears it.
            regs::GPTC | regs::GPRC | regs::TPR | regs::MPC | regs::CRCERRS => {
                let count = self.get(reg);
                self.set(reg, 0);
                count
            }
            _ => self.get(reg),
        }
    }

    fn write(&mut self, reg: usize, value: u32) {
        // A write over the bus takes as long as a read of it, and a bound
        // measured from one is short by that much if the write is free.
        self.nanos += CLOCK_STEP_NANOS;
        assert!(
            reg.is_multiple_of(4) && reg + 4 <= regs::REGISTER_BYTES,
            "seed {}: a {reg:#x} register write is outside the file",
            self.seed
        );
        self.keep_quiet(reg);
        self.written.insert(reg);
        assert!(
            self.not_decoding != Some(reg),
            "seed {}: the driver wrote {value:#010x} into {reg:#x}, which nothing decodes — the \
             ones it read back there were never a register's value to carry",
            self.seed
        );
        if self.refusing == Some(reg) {
            return;
        }
        match reg {
            regs::CTRL => {
                let was = self.get(regs::CTRL);
                assert_eq!(
                    value & CTRL_RESERVED_SET,
                    CTRL_RESERVED_SET,
                    "seed {}: the driver wrote {value:#010x} to CTRL, clearing a reserved bit \
                     §10.2.2.1 documents as set",
                    self.seed
                );
                self.set(regs::CTRL, value);
                self.lanphypc(was, value);
                if value & !was & ctrl::GIO_MASTER_DISABLE != 0 {
                    // §3.1.3.10: the part "blocks new master requests [...] then
                    // proceeds to issue any pending requests by this function".
                    self.master_reads = if self.master_sticks {
                        u32::MAX
                    } else if self.permits.master_takes_time_to_quiesce {
                        MASTER_QUIESCE_READS
                    } else {
                        0
                    };
                }
                if value & ctrl::RST != 0 {
                    assert!(
                        was & ctrl::GIO_MASTER_DISABLE != 0,
                        "seed {}: the driver reset a function still mastering, and §10.2.2.1 \
                         says \"before issuing this reset, software has to insure that Tx and \
                         Rx processes are stopped by following the procedure described in \
                         Section 3.1.3.10\"",
                        self.seed
                    );
                    self.reset_at = self.nanos;
                    self.lcd_reset_at = Some(self.nanos);
                    let with_the_phy = value & ctrl::PHY_RST != 0;
                    if with_the_phy {
                        assert_ne!(
                            self.get(regs::FWSM) & fwsm::PHY_RESET_ALLOWED,
                            0,
                            "seed {}: the driver reset the PHY with FWSM {:#010x}, whose bit 6 \
                             says the firmware blocks it",
                            self.seed,
                            self.get(regs::FWSM)
                        );
                        self.phy_resets += 1;
                        if self.lcd == Lcd::OutOfStep {
                            self.lcd = Lcd::InStep;
                        }
                        self.phy_configured_reads = Some(PHY_CONFIGURED_READS);
                        self.quiet_until = Some(self.nanos + wake::RESET_QUIET_NANOS);
                    } else if self.part == Part::I219
                        && self.permits.mac_reset_alone_loses_the_phy
                        && self.lcd == Lcd::InStep
                    {
                        self.lcd = Lcd::OutOfStep;
                    }
                    // §10.2.2.1: "a reset of the MAC function of the device".
                    // Everything but the reserved defaults and the NVM's
                    // station address goes — §4.5.2's ownership bits with it.
                    self.power_on();
                    self.phy.sw_requested = false;
                    self.refresh_ownership();
                    self.set(regs::CTRL, self.get(regs::CTRL) | ctrl::RST);
                    self.reset_reads = RESET_READS;
                }
                self.refresh_status();
            }
            // §10.2.4.5: set, not assign.
            regs::IMS => {
                let held = self.get(regs::IMS);
                self.set(regs::IMS, held | value);
            }
            // §10.2.4.6: clear.
            regs::IMC => {
                let held = self.get(regs::IMS);
                self.set(regs::IMS, held & !value);
            }
            // §10.2.4.4: set a cause, as if the event had happened.
            regs::ICS => self.raise(value),
            // §10.2.4.1: "Writing a 1b to any bit in the register also clears
            // that bit. Writing a 0b to any bit has no effect on that bit."
            // `INT_ASSERTED` is not writable and clears with its causes.
            regs::ICR => {
                let held = self.get(regs::ICR);
                let mut left = held & !(value & !cause::INT_ASSERTED);
                if left & !cause::INT_ASSERTED == 0 {
                    left = 0;
                }
                self.set(regs::ICR, left);
            }
            // §4.5.2: "A request for ownership is registered by writing a 1b
            // into the respective bit", and it stands until the agent writes a
            // 0b back — so one write registers it and the reads after it are
            // how the agent learns it was granted.
            regs::EXTCNF_CTRL => {
                self.not_modelled_on_the_82574("EXTCNF_CTRL");
                self.arbitration_at.push(self.nanos);
                let carried = self.get(regs::EXTCNF_CTRL) & !extcnf::OWNERSHIP;
                assert_eq!(
                    value & !extcnf::OWNERSHIP,
                    carried,
                    "seed {}: the driver wrote {value:#010x} to EXTCNF_CTRL, of which §4.5.2 \
                     gives it one bit — the rest of the register is another agent's and this \
                     write changed it from {carried:#010x}",
                    self.seed
                );
                self.phy.extcnf_writes += 1;
                self.phy.sw_requested = value & extcnf::MDIO_SW_OWNERSHIP != 0;
                if self.phy.sw_requested {
                    if let Some(reads) = self.phy.extcnf_ones_after_request.take() {
                        self.phy.extcnf_ones = reads;
                    }
                }
                self.refresh_ownership();
            }
            regs::MDIC => self.mdi(value),
            regs::CTRL_EXT => {
                let was = self.get(regs::CTRL_EXT);
                if value & !was & ctrl_ext::MAC_ON_SMBUS != 0 {
                    self.smbus_forced_at = Some(self.nanos);
                    if self.phy.transition_sticks_on_smbus {
                        self.phy.wait_sticks = true;
                        self.phy.wait_reads = u32::MAX;
                    }
                }
                self.set(regs::CTRL_EXT, value);
            }
            // §10.2.2.2: read-only.
            regs::STATUS => {}
            regs::RDT => {
                self.set(regs::RDT, value);
                if !self.held {
                    self.run();
                }
            }
            regs::TDT => {
                self.set(regs::TDT, value);
                if !self.held {
                    self.run();
                }
            }
            _ => self.set(reg, value),
        }
    }

    /// The 20 ms after a full reset, in which the part takes no access.
    fn keep_quiet(&mut self, reg: usize) {
        if let Some(until) = self.quiet_until.take() {
            assert!(
                self.nanos >= until,
                "seed {}: the driver reached register {reg:#x} {} ns after a reset of the MAC and \
                 the PHY together, inside the 20 ms nothing is touched",
                self.seed,
                wake::RESET_QUIET_NANOS - (until - self.nanos)
            );
        }
    }

    /// `CTRL`'s hand on `LANPHYPC`: driven low with the override, a power cycle
    /// once the override is let go.
    fn lanphypc(&mut self, was: u32, now: u32) {
        let low = |word: u32| {
            word & ctrl::LANPHYPC_HOST_DRIVEN != 0 && word & ctrl::LANPHYPC_LEVEL == 0
        };
        if low(now) && !low(was) {
            assert_eq!(
                self.get(regs::LANPHYPC_TIMING) & lanphypc_timing::CONFIGURATION_MASK,
                lanphypc_timing::CONFIGURATION_50MS,
                "seed {}: the driver drove LANPHYPC low with the PHY's configuration time not at \
                 50 ms",
                self.seed
            );
            self.lanphypc_low_at = Some(self.nanos);
        }
        if now & ctrl::LANPHYPC_HOST_DRIVEN == 0 {
            if let Some(at) = self.lanphypc_low_at.take() {
                assert!(
                    self.nanos - at >= wake::LANPHYPC_HOLD_NANOS,
                    "seed {}: the driver held LANPHYPC low {} ns, under the 10 us a power cycle \
                     takes",
                    self.seed,
                    self.nanos - at
                );
                self.power_cycle();
            }
        }
    }

    /// I219 Table 5-1's internal power-on reset: every PHY register back to
    /// its default, and the PHY back on whichever interface it resets to.
    ///
    /// **The identifier is the silicon's and not a register's state**, so what
    /// a test made the part identify as survives the power it loses.
    fn power_cycle(&mut self) {
        self.power_cycles += 1;
        self.lcd = if self.permits.phy_comes_back_on_smbus { Lcd::Smbus } else { Lcd::InStep };
        self.lcd_reset_at = Some(self.nanos);
        let mut defaults = PhyModel::new(&self.permits.after_a_phy_reset());
        for identifier in [reg::IDENTIFIER_HIGH, reg::IDENTIFIER_LOW] {
            defaults.file[identifier as usize] = self.phy.file[identifier as usize];
        }
        self.phy.file = defaults.file;
        self.phy.custom_mode = defaults.custom_mode;
        // §9.5.3.2: "This bit is reset by power on reset only" — and a power
        // cycle is one.
        self.phy.port_general = PORT_GENERAL_DEFAULT;
        self.phy.page = None;
        self.phy.negotiated_over = None;
        self.phy.negotiating = false;
        let ext = self.get(regs::CTRL_EXT) & !ctrl_ext::LANPHYPC_CYCLE_DONE;
        self.set(regs::CTRL_EXT, ext);
        self.cycle_done_reads = CYCLE_DONE_READS;
        self.refresh_phy_link();
    }

    /// §4.5.2's arbitration, answered into the register the requester reads it
    /// out of: "the priority order is manageability, software and then
    /// hardware", and "at any given time at most only one bit is 1b".
    fn refresh_ownership(&mut self) {
        let engine_asks = self.phy.mdio_sticks || self.phy.firmware_requests > 0;
        let (engine, software) = if self.permits.mdio_flag_is_a_plain_mutex {
            // An ordinary mutex bit: it reads back set for whichever agent set
            // it and says nothing at all about a grant, so the engine's own
            // ownership stands beside it.
            (engine_asks, self.phy.sw_requested || self.phy.flag_held_by_another_agent)
        } else if self.phy.flag_held_by_another_agent {
            // Another agent holds the grant, and "at any given time at most
            // only one bit is 1b": the engine's request waits behind it.
            (false, true)
        } else {
            // §4.5.2: "access is not granted as long as the bit is 0b", so the
            // bit stands only once the arbitration has granted the request.
            (engine_asks, !engine_asks && self.phy.sw_requested)
        };
        // §4.5.2's third agent. Under the mutex reading its bit stands beside
        // whatever else does, the same way the software flag does; under the
        // grant reading "the priority order is manageability, software and then
        // hardware", so it stands only where neither of the others does.
        let hardware = self.phy.hardware_holds
            && (self.permits.mdio_flag_is_a_plain_mutex || !(engine || software));
        let bits = if engine { extcnf::MDIO_MNG_OWNERSHIP } else { 0 }
            | if software { extcnf::MDIO_SW_OWNERSHIP } else { 0 }
            | if hardware { extcnf::MDIO_HW_OWNERSHIP } else { 0 };
        if self.phy.ownership_readings.last() != Some(&bits) {
            self.phy.ownership_readings.push(bits);
        }
        // The arbitration answers three bits and touches nothing else: what
        // stands in the rest of the register outlives every access to it.
        let carried = self.get(regs::EXTCNF_CTRL) & !extcnf::OWNERSHIP;
        self.set(regs::EXTCNF_CTRL, carried | bits);
    }

    /// A register this file models only behind the PCH part. §10.2.2.7
    /// addresses the 82574's own PHY as "1 = Gigabit PHY. 2 = PCIe PHY" and
    /// none of that register set is here, so a driver that reached one on that
    /// part would be driving a map this model does not have.
    fn not_modelled_on_the_82574(&self, named: &str) {
        assert_eq!(
            self.part,
            Part::I219,
            "seed {}: the driver reached {named} on the 82574, whose PHY this model does not \
             implement",
            self.seed
        );
    }

    /// One MDI transaction, as §10.2.2.7 defines its two sequences.
    fn mdi(&mut self, command: u32) {
        self.not_modelled_on_the_82574("MDIC");
        // §9.2: "After LCD reset to the I219 a delay of 10 ms is required
        // before attempting to access MDIO registers."
        if let Some(at) = self.lcd_reset_at {
            assert!(
                self.nanos.saturating_sub(at) >= crate::phy::LCD_RESET_DELAY_NANOS,
                "seed {}: the driver started an MDI transaction {} ns after it reset the PHY, and \
                 §9.2 requires 10 ms",
                self.seed,
                self.nanos.saturating_sub(at)
            );
        }
        assert!(
            command & mdic::READY == 0,
            "seed {}: the driver wrote an MDI command with the Ready bit already set, and \
             §10.2.2.7 says it \"should be reset to 0b by software at the same time the \
             command is written\"",
            self.seed
        );
        assert!(
            command & mdic::ERROR == 0,
            "seed {}: the driver wrote an MDI command with the Error bit set, and §10.2.2.7 \
             says software \"should make sure this bit is clear (0b) before making an MDI \
             Read or Write command\"",
            self.seed
        );
        assert!(
            command & mdic::INTERRUPT == 0,
            "seed {}: the driver asked for §10.2.2.7's end-of-cycle interrupt, which nothing \
             in this driver reads",
            self.seed
        );
        // §4.5.2: "The requesting agent is granted access when the same bit is
        // read as 1b", so a transaction driven with the software bit clear is
        // one driven on an interface this driver was never granted — and on
        // this part the agent it would be racing is the Management Engine,
        // driving the same PHY through the same pins.
        //
        // **The clause's "at any given time at most only one bit is 1b" is not
        // asserted, because the part does not honour it**: on the T14 the grant
        // came back as `0x003000a9`, this driver's bit set with bit 7 still
        // standing. A model that demanded the bit alone would refuse the one
        // bring-up the silicon allows.
        assert_ne!(
            self.get(regs::EXTCNF_CTRL) & extcnf::MDIO_SW_OWNERSHIP,
            0,
            "seed {}: the driver started an MDI transaction with {:#010x} standing in §4.5.2's \
             three ownership bits, its own software bit clear among them",
            self.seed,
            self.get(regs::EXTCNF_CTRL) & extcnf::OWNERSHIP
        );
        // §8.2.3: "The ME/Host should not issue new MDIC transactions while
        // this bit is set to 1."
        assert_eq!(
            self.phy.wait_reads, 0,
            "seed {}: the driver issued an MDI transaction while §8.2.3's Wait bit was standing",
            self.seed
        );
        let addr = ((command >> mdic::PHYADD_SHIFT) & mdic::ADDRESS_MASK) as u8;
        let reg = ((command >> mdic::REGADD_SHIFT) & mdic::ADDRESS_MASK) as u8;
        let data = (command & mdic::DATA_MASK) as u16;
        let write = match command & (mdic::OP_READ | mdic::OP_WRITE) {
            mdic::OP_WRITE => true,
            mdic::OP_READ => false,
            other => panic!(
                "seed {}: the driver wrote op-code {:#b} to MDIC, and §10.2.2.7 allows 01b and \
                 10b and says \"other values are reserved\"",
                self.seed,
                other >> 26
            ),
        };
        // §8.2.1's PHYPDN standing is the PHY held in power down, and I219
        // §2.2's Table 2-1 puts the interconnect in Electrical Idle for "S0 and
        // PHY Power Down" — so the in-band MDIO packet I219 §1.2 and §2.2.2.1.1
        // carry this transaction on goes nowhere, nothing acknowledges it, and
        // neither `Ready` nor `Error` ever comes back. **This is the wall the
        // bench boot is reading, modelled**: a driver that skipped the power
        // step gets exactly it.
        let forced = self.get(regs::CTRL_EXT) & ctrl_ext::MAC_ON_SMBUS != 0;
        if forced {
            let at = self.smbus_forced_at.expect("the force was written");
            assert!(
                self.nanos - at >= wake::SMBUS_SETTLE_NANOS,
                "seed {}: the driver started an MDI transaction {} ns after moving the MAC onto \
                 SMBus, inside the 50 ms in which the MAC carries no cycle over it",
                self.seed,
                self.nanos - at
            );
        }
        // The interconnect carries the cycle only where both ends are on the
        // same interface and in step.
        let reachable = matches!((self.lcd, forced), (Lcd::InStep, false) | (Lcd::Smbus, true));
        let gated = self.get(regs::CTRL) & ctrl::PHY_POWER_DOWN != 0 || !reachable;
        self.phy.mdi_reads = if self.phy.never_ready || gated {
            u32::MAX
        } else if self.permits.mdi_takes_several_reads {
            MDI_READS
        } else {
            0
        };
        if gated {
            self.mdi_answer = command;
            return;
        }
        if !write && self.phy.failing == Some((addr, reg)) {
            // §10.2.2.7's `Error`: a read the part "fails to complete". `Ready`
            // is set with it, because the same section sets that bit "at the
            // end of the MDI transaction" and a failed read is one that ended —
            // so what stands in the data field is not what the PHY said.
            self.mdi_answer = command | mdic::ERROR | mdic::READY;
            return;
        }
        // The write that moves the PHY between its two interfaces ends in
        // `Error` by nature, whichever way it moves it.
        if write && matches!(self.phy_at(addr, reg), PhyPlace::SmbusControl) {
            self.phy_write(addr, reg, data);
            self.mdi_answer = command | mdic::ERROR | mdic::READY;
            return;
        }
        let answered = if self.phy.deaf == Some(addr) {
            // Nothing drives the bus at this address, so the transaction
            // completes and the data lines float high.
            u16::MAX
        } else if write {
            self.phy_write(addr, reg, data);
            data
        } else {
            self.phy_read(addr, reg)
        };
        self.mdi_answer = (command & !mdic::DATA_MASK) | mdic::READY | answered as u32;
    }

    /// Where one PHY register lives, as Table 9-1 of the I219's document places
    /// it: PHY address 02 for the registers §9.5.2 defines, PHY address 01 and
    /// a selected page for everything else.
    fn phy_at(&self, addr: u8, reg: u8) -> PhyPlace {
        match (addr, reg) {
            (phy::SPECIFIC, r) if r < FIRST_PAGED_REGISTER => PhyPlace::Ieee(r),
            (phy::GENERAL, reg::PAGE_SELECT) => PhyPlace::Page,
            (phy::GENERAL, r) if r < FIRST_PAGED_REGISTER => PhyPlace::Ieee(r),
            (phy::GENERAL, wake::SMBUS_CONTROL) => {
                assert_eq!(
                    self.phy.page,
                    Some(phy::PAGE_PORT_CONTROL),
                    "seed {}: the driver reached §9.5.3.4's SMBus Control with page {:?} selected",
                    self.seed,
                    self.phy.page
                );
                PhyPlace::SmbusControl
            }
            (phy::GENERAL, reg::PORT_GENERAL) => {
                assert_eq!(
                    self.phy.page,
                    Some(phy::PAGE_PORT_CONTROL),
                    "seed {}: the driver reached §9.5.3.2's Port General Configuration with page \
                     {:?} selected",
                    self.seed,
                    self.phy.page
                );
                PhyPlace::PortGeneral
            }
            (phy::GENERAL, reg::CUSTOM_MODE) => {
                assert_eq!(
                    self.phy.page,
                    Some(phy::PAGE_PORT_CONTROL),
                    "seed {}: the driver read or wrote register {reg} at PHY address 01 with \
                     page {:?} selected, and §9.3 says every register above 15 there is the \
                     page's",
                    self.seed,
                    self.phy.page
                );
                PhyPlace::CustomMode
            }
            (a, _) if a != phy::GENERAL && a != phy::SPECIFIC => PhyPlace::Undriven,
            _ => panic!(
                "seed {}: the driver reached PHY address {addr:02} register {reg}, which \
                 Table 9-1 of the I219's document does not place and this model does not have",
                self.seed
            ),
        }
    }

    fn phy_read(&mut self, addr: u8, reg: u8) -> u16 {
        match self.phy_at(addr, reg) {
            PhyPlace::Undriven => u16::MAX,
            PhyPlace::Page => self.phy.page.unwrap_or(0) << phy::PAGE_SHIFT,
            PhyPlace::CustomMode => self.phy.custom_mode,
            PhyPlace::PortGeneral => self.phy.port_general,
            PhyPlace::SmbusControl => {
                if self.lcd == Lcd::Smbus {
                    wake::SMBUS_CONTROL_FORCE
                } else {
                    0
                }
            }
            PhyPlace::Ieee(r) => self.phy.file[r as usize],
        }
    }

    /// §9.1: "Other fields in the same 16-bit register must be loaded with
    /// their default values." So a write that changed a field it is not about
    /// is a driver composing a register it should have read first, and the
    /// defaults it had to carry are §9.5.2's and §9.5.3's own tables.
    fn carried(&self, named: &str, data: u16, table: &Carried) {
        let (mask, default) = (table.mask, table.default);
        assert_eq!(
            data & mask,
            default,
            "seed {}: the driver wrote {data:#06x} to {named}, whose other fields §9.1 says \
             \"must be loaded with their default values\" — {:#06x} under the mask {mask:#06x}",
            self.seed,
            default
        );
    }

    fn phy_write(&mut self, addr: u8, reg: u8, data: u16) {
        match self.phy_at(addr, reg) {
            // Nothing takes it, and the transaction still ends.
            PhyPlace::Undriven => {}
            // §9.3: "only the 11 MSBs of register 31 are used for defining the
            // page. During write to the page register, the five LSBs are
            // ignored."
            PhyPlace::Page => self.phy.page = Some(data >> phy::PAGE_SHIFT),
            PhyPlace::SmbusControl => {
                // §9.5.3.4's other fields come up 0b, and this model leaves them
                // there.
                assert_eq!(
                    data & !wake::SMBUS_CONTROL_FORCE,
                    0,
                    "seed {}: the driver wrote {data:#06x} to §9.5.3.4's SMBus Control, whose \
                     other fields it read as 0b",
                    self.seed
                );
                self.lcd = if data & wake::SMBUS_CONTROL_FORCE != 0 {
                    Lcd::Smbus
                } else {
                    Lcd::InStep
                };
            }
            PhyPlace::PortGeneral => {
                let wake = phy::port_general::HOST_WAKE_UP_ACTIVE;
                assert_eq!(
                    data & !wake,
                    self.phy.port_general & !wake,
                    "seed {}: the driver wrote {data:#06x} to §9.5.3.2's Port General \
                     Configuration, which held {:#06x}: §9.1 has every field it does not mean \
                     to move loaded with what is there",
                    self.seed,
                    self.phy.port_general
                );
                // §9.5.3.2: `Host_WU_Active` "is not blocked for writes" only
                // while `MACPD_enable` is set.
                let movable = self.phy.port_general & phy::port_general::MACPD_ENABLE != 0;
                if movable {
                    self.phy.port_general = data;
                }
            }
            PhyPlace::CustomMode => {
                self.carried("§9.5.3.1's Custom Mode Control", data, &carried::CUSTOM_MODE);
                self.phy.custom_mode = data;
                self.refresh_phy_link();
            }
            PhyPlace::Ieee(r) => {
                if r == reg::CONTROL {
                    self.carried("§9.5.2.1's Control register", data, &carried::CONTROL);
                }
                if r == reg::ADVERTISE {
                    self.carried(
                        "§9.5.2.5's Auto-Negotiation Advertisement",
                        data,
                        &carried::ADVERTISE,
                    );
                }
                if r == reg::CONTROL_1000T {
                    self.carried("§9.5.2.10's 1000BASE-T Control", data, &carried::CONTROL_1000T);
                }
                if r == reg::CONTROL && data & control::RESET != 0 {
                    // §9.5.2.1: "Writing a 1b to this bit causes immediate PHY
                    // reset", so every register below goes back to the default
                    // §9.5 prints for it.
                    let defaults = PhyModel::new(&self.permits.after_a_phy_reset());
                    self.phy.file = defaults.file;
                    self.phy.custom_mode = defaults.custom_mode;
                    self.phy.negotiated_over = None;
                    self.phy.negotiating = false;
                    self.refresh_phy_link();
                    return;
                }
                let abilities = |model: &PhyModel| {
                    (
                        model.file[reg::ADVERTISE as usize],
                        model.file[reg::CONTROL_1000T as usize],
                    )
                };
                self.phy.file[r as usize] = data;
                if r == reg::ADVERTISE {
                    // §9.5.2.5: "Any write to the Auto-Negotiation
                    // Advertisement register, prior to auto-negotiation
                    // completion, is followed by a restart of
                    // auto-negotiation."
                    self.phy.negotiated_over = Some(abilities(&self.phy));
                    self.phy.negotiating = true;
                }
                if r == reg::CONTROL && data & control::RESTART_AUTONEG != 0 {
                    self.phy.negotiated_over = Some(abilities(&self.phy));
                    self.phy.negotiating = true;
                    // §9.5.2.1: the bit is `RW/SC`.
                    self.phy.file[reg::CONTROL as usize] &= !control::RESTART_AUTONEG;
                }
                self.refresh_phy_link();
            }
        }
    }

    /// Record a cause and, if it is unmasked *and* has a vector, send a
    /// message.
    ///
    /// §10.2.4.1 gives every event two names: the classic cause and the
    /// queue-or-other cause §10.2.4.9 allocates a vector to. Both are set in
    /// `ICR`; only the second can reach a vector, and only while `IVAR`'s
    /// enable bit for it is set. **A driver that programmed no `IVAR` is
    /// therefore told nothing at all**, which is the part behaving as specified
    /// and not a model being unkind.
    fn raise(&mut self, causes: u32) {
        let held = self.get(regs::ICR);
        let mut now = held | causes;
        if causes & (cause::RXT0 | cause::RXDMT0) != 0 {
            now |= cause::RXQ0;
        }
        if causes & cause::TXDW != 0 {
            now |= cause::TXQ0;
        }
        if causes & (cause::LSC | cause::RXO) != 0 {
            now |= cause::OTHER;
        }
        let unmasked = now & self.get(regs::IMS) & !cause::INT_ASSERTED;
        let delivered = unmasked & self.vectored();
        self.set(regs::ICR, if delivered != 0 { now | cause::INT_ASSERTED } else { now });
        if delivered != 0 {
            self.messages = self.messages.saturating_add(1);
        }
    }

    /// The causes `IVAR` has a valid entry for, and therefore the only ones
    /// that reach a vector (§10.2.4.9).
    fn vectored(&self) -> u32 {
        let allocation = self.get(regs::IVAR);
        let mut causes = 0;
        for (valid, which) in [
            (3, cause::RXQ0),
            (7, cause::RXQ1),
            (11, cause::TXQ0),
            (15, cause::TXQ1),
            (19, cause::OTHER),
        ] {
            if allocation & (1 << valid) != 0 {
                causes |= which;
            }
        }
        causes
    }

    /// Turn a device address in a descriptor into a grant offset, the way the
    /// unit does: an address this function was never granted reaches nothing.
    fn resolve(&self, address: u64) -> usize {
        assert!(
            address >= DEVICE_BASE && address < DEVICE_BASE + self.memory.len() as u64,
            "seed {}: the driver put {address:#x} in a descriptor, which is not an address \
             this function was granted — on a real machine the unit refuses it and the \
             kernel records a DMA fault against the process",
            self.seed
        );
        (address - DEVICE_BASE) as usize
    }

    /// Where the receive descriptor ring is, as the registers say.
    fn rx_ring(&self) -> usize {
        self.resolve(((self.get(regs::RDBAH) as u64) << 32) | self.get(regs::RDBAL) as u64)
    }

    /// Where the transmit descriptor ring is, as the registers say.
    fn tx_ring(&self) -> usize {
        self.resolve(((self.get(regs::TDBAH) as u64) << 32) | self.get(regs::TDBAL) as u64)
    }

    fn desc_read(&self, at: usize) -> u64 {
        u64::from_le_bytes(self.memory[at..at + 8].try_into().unwrap())
    }

    fn desc_write(&mut self, at: usize, word: u64) {
        self.memory[at..at + 8].copy_from_slice(&word.to_le_bytes());
    }

    /// One tick of the device: place what has arrived, send what has been
    /// published, and write back what it is holding.
    fn run(&mut self) {
        self.receive();
        self.transmit();
        self.flush_rx();
        self.flush_tx();
        if self.permits.spurious_interrupts && next(&mut self.rng).is_multiple_of(32) {
            // §7.4.5: a message whose cause is already gone. Nothing is set in
            // `ICR`, which is exactly what makes it spurious.
            self.messages = self.messages.saturating_add(1);
        }
    }

    /// One more in a statistics register.
    fn count(&mut self, reg: usize) {
        let held = self.get(reg);
        self.set(reg, held.saturating_add(1));
    }

    /// Place frames into descriptors, up to what `RDT` made available.
    fn receive(&mut self) {
        if self.get(regs::RCTL) & rctl::EN == 0 {
            return;
        }
        let ring = self.rx_ring();
        let count = self.get(regs::RDLEN) as usize / rx_desc::BYTES;
        if count == 0 {
            return;
        }
        let tail = self.get(regs::RDT) as usize % count;
        let strip_crc = self.get(regs::RCTL) & rctl::SECRC != 0;
        while self.rx_head != tail {
            let Some(frame) = self.inbound.pop_front() else { return };
            self.count(regs::TPR);
            let at = ring + self.rx_head * rx_desc::BYTES;
            let buffer = self.desc_read(at);
            // §7.1.7.2: a null data address is stored into and written back
            // with `DD` alone.
            if buffer == 0 {
                if !self.permits.null_padding {
                    return;
                }
                let word = (rx_desc::status::DD as u64) << rx_desc::STATUS_SHIFT;
                self.rx_holding.push((self.rx_head, word, Vec::new()));
                self.rx_head = (self.rx_head + 1) % count;
                // Not a frame the MAC took yet: it is still waiting.
                let seen = self.get(regs::TPR);
                self.set(regs::TPR, seen - 1);
                self.inbound.push_front(frame);
                continue;
            }
            let into = self.resolve(buffer);
            // §10.2.5.1: `LPE` is clear, so a frame over 1522 bytes is
            // discarded by the hardware and never spans two buffers.
            if frame.len() > 1522 {
                continue;
            }
            let stored = if strip_crc { frame.len() } else { frame.len() + 4 };
            assert!(
                stored <= crate::RX_BUF_BYTES,
                "seed {}: the model was asked to store {stored} bytes in a \
                 {}-byte buffer",
                self.seed,
                crate::RX_BUF_BYTES
            );
            // The buffer is the device's until its descriptor says otherwise,
            // so a driver that reads it before `DD` reads this and not a frame.
            for byte in self.memory[into..into + crate::RX_BUF_BYTES].iter_mut() {
                *byte = 0xDE;
            }
            let word = (stored as u64 & rx_desc::LENGTH_MASK)
                | ((rx_desc::status::DD | rx_desc::status::EOP) as u64)
                    << rx_desc::STATUS_SHIFT;
            self.count(regs::GPRC);
            self.rx_holding.push((self.rx_head, word, frame));
            self.rx_head = (self.rx_head + 1) % count;
        }
    }

    /// Write back the descriptors the device is holding.
    ///
    /// §7.1.7.1 lets it accumulate them and write them "in cache line-oriented
    /// chunks"; §7.1.8 says the head advances "just prior to" — but the head
    /// register is a shadow that may already count descriptors "not yet stored
    /// in memory", so the register moves first when [`Permits::shadow_head`]
    /// is on.
    fn flush_rx(&mut self) {
        if self.rx_holding.is_empty() {
            return;
        }
        let ring = self.rx_ring();
        let batch = if self.permits.batched_writeback {
            let chunk = 1 + (next(&mut self.rng) % 4) as usize;
            chunk.min(self.rx_holding.len())
        } else {
            self.rx_holding.len()
        };
        let mut holding: Vec<(usize, u64, Vec<u8>)> = self.rx_holding.drain(..batch).collect();
        if self.permits.shadow_head {
            // The register counts what the device has finished with, whether or
            // not memory says so yet. A driver reading `RDH` to find
            // completions reads descriptors that are not there.
            self.set(regs::RDH, self.rx_head as u32);
        }
        if self.permits.batched_writeback {
            // The order inside a chunk is the hardware's, so a driver may not
            // infer an earlier descriptor's state from a later one's.
            let rounds = holding.len();
            for i in 0..rounds {
                let j = i + (next(&mut self.rng) as usize % (rounds - i));
                holding.swap(i, j);
            }
        }
        for (index, word, frame) in holding {
            if !frame.is_empty() {
                let buffer = self.desc_read(ring + index * rx_desc::BYTES);
                let into = self.resolve(buffer);
                self.memory[into..into + frame.len()].copy_from_slice(&frame);
            }
            self.desc_write(ring + index * rx_desc::BYTES + 8, word);
        }
        if !self.permits.shadow_head {
            self.set(regs::RDH, self.rx_head as u32);
        }
        self.raise(cause::RXT0);
    }

    /// Take every published transmit descriptor and put its frame on the wire.
    fn transmit(&mut self) {
        if self.get(regs::TCTL) & tctl::EN == 0 {
            return;
        }
        let ring = self.tx_ring();
        let count = self.get(regs::TDLEN) as usize / tx_desc::BYTES;
        if count == 0 {
            return;
        }
        let tail = self.get(regs::TDT) as usize % count;
        // §7.2.4.1: "The 82574 NEVER fetches descriptors beyond the descriptor
        // tail pointer."
        while self.tx_head != tail {
            let at = ring + self.tx_head * tx_desc::BYTES;
            let buffer = self.desc_read(at);
            let word = self.desc_read(at + 8);
            let command = ((word >> tx_desc::CMD_SHIFT) & 0xFF) as u8;
            assert!(
                command & tx_desc::cmd::DEXT == 0,
                "seed {}: the driver published a descriptor with DEXT set, and §7.2.10 \
                 says an extended descriptor's fields mean something else entirely",
                self.seed
            );
            assert!(
                command & tx_desc::cmd::EOP != 0,
                "seed {}: the driver published a transmit descriptor with no EOP, so this \
                 frame would never be put on the wire",
                self.seed
            );
            let len = (word & tx_desc::LENGTH_MASK) as usize;
            let from = self.resolve(buffer);
            self.sent.push(self.memory[from..from + len].to_vec());
            self.count(regs::GPTC);
            if command & tx_desc::cmd::RS != 0 {
                self.tx_holding.push(self.tx_head);
            }
            self.tx_head = (self.tx_head + 1) % count;
        }
    }

    /// §7.2.4.2: with `RS` set and no `IDE`, "The device writes back only the
    /// status byte of the descriptor (TDESCR.STA) and all other bytes of the
    /// descriptor are left unchanged" — and it may hold a batch first.
    fn flush_tx(&mut self) {
        if self.tx_holding.is_empty() {
            return;
        }
        let ring = self.tx_ring();
        let batch = if self.permits.batched_writeback {
            let chunk = 1 + (next(&mut self.rng) % 4) as usize;
            chunk.min(self.tx_holding.len())
        } else {
            self.tx_holding.len()
        };
        let holding: Vec<usize> = self.tx_holding.drain(..batch).collect();
        self.set(regs::TDH, self.tx_head as u32);
        for index in holding {
            let at = ring + index * tx_desc::BYTES + 8;
            let word = self.desc_read(at);
            self.desc_write(
                at,
                word | ((tx_desc::STATUS_DD as u64) << tx_desc::STATUS_SHIFT),
            );
        }
        self.raise(cause::TXDW);
    }
}

/// The part, and the four views a driver is built on.
///
/// One model behind all four, because on a machine they are one device.
#[derive(Clone)]
pub struct Nic(Rc<RefCell<Model>>);

impl Nic {
    /// The 82574 — the part QEMU's `e1000e` models, whose own PHY register map
    /// this file does not have.
    pub fn new(seed: u64) -> Self {
        Self::with(seed, Part::E82574, Permits::default())
    }

    /// The ThinkPad's `8086:15fc`, whose PHY is the one §9 describes.
    pub fn i219(seed: u64) -> Self {
        Self::with(seed, Part::I219, Permits::default())
    }

    pub fn with(seed: u64, part: Part, permits: Permits) -> Self {
        Self(Rc::new(RefCell::new(Model::new(seed, part, permits))))
    }

    pub fn seed(&self) -> u64 {
        self.0.borrow().seed
    }

    pub fn part(&self) -> Part {
        self.0.borrow().part
    }

    /// The first clause of §9 the PHY has not been given, or `None` where it
    /// can raise a link. **What a link this model refuses to raise is refused
    /// for**: a bring-up that leaves out one register write is named here
    /// rather than read as a quiet wire.
    pub fn phy_unconfigured(&self) -> Option<&'static str> {
        self.0.borrow().phy_unconfigured()
    }

    /// §4.5.2's arbitration never grants this driver the MDIO interface — what
    /// a Management Engine that holds the PHY for the boot looks like.
    pub fn mdio_never_granted(&self) {
        self.0.borrow_mut().phy.mdio_sticks = true;
    }

    /// §3.1.3.10's master enable never goes quiet, which the same section
    /// sanctions a driver timing out on.
    pub fn master_never_quiesces(&self) {
        self.0.borrow_mut().master_sticks = true;
    }

    /// §10.2.2.7's `Ready` bit never comes back.
    /// §8.2.3's `Wait` set and never auto-cleared: an interconnect transition
    /// that does not finish, which is the one state the clause says a host may
    /// not issue an MDIC transaction in.
    pub fn interconnect_never_leaves_its_transition(&self) {
        let mut model = self.0.borrow_mut();
        model.phy.wait_sticks = true;
        model.phy.wait_reads = u32::MAX;
    }

    pub fn mdi_never_ready(&self) {
        self.0.borrow_mut().phy.never_ready = true;
    }

    /// §4.5.2's third agent holds the interface: the part's own hardware, which
    /// takes one "while loading the extended configuration area" and whose
    /// bit 6 no driver can write.
    pub fn hardware_holds_the_mdio_interface(&self) {
        self.0.borrow_mut().phy.hardware_holds = true;
    }

    /// §4.5.2's three ownership bits as this part answered them, one entry per
    /// change — the premise a test about *which* reading a refusal carries has
    /// to establish before it asserts on one.
    pub fn ownership_readings(&self) -> Vec<u32> {
        self.0.borrow().phy.ownership_readings.clone()
    }

    /// The modelled clock at every access this driver made to `EXTCNF_CTRL`, in
    /// the order it made them.
    pub fn arbitration_at(&self) -> Vec<u64> {
        self.0.borrow().arbitration_at.clone()
    }

    /// Another agent holds §4.5.2's software flag when this claim is minted.
    pub fn mdio_flag_held_by_another_agent(&self) {
        self.0.borrow_mut().phy.flag_held_by_another_agent = true;
    }

    /// The engine writes its ownership bit back to 0b, and §4.5.2's
    /// arbitration answers whatever request is still registered.
    pub fn engine_lets_go(&self) {
        let mut model = self.0.borrow_mut();
        model.phy.mdio_sticks = false;
        model.refresh_ownership();
    }

    /// How many times the engine registered its request right after the
    /// interface was read free.
    pub fn engine_cut_ins(&self) -> u32 {
        self.0.borrow().phy.engine_cut_ins
    }

    /// The next `reads` reads of `EXTCNF_CTRL` answer ones, from now.
    pub fn extcnf_answers_ones_for(&self, reads: u32) {
        self.0.borrow_mut().phy.extcnf_ones = reads;
    }

    /// The first `reads` reads of `EXTCNF_CTRL` after this driver's request is
    /// written answer ones.
    pub fn extcnf_answers_ones_after_the_request(&self, reads: u32) {
        self.0.borrow_mut().phy.extcnf_ones_after_request = Some(reads);
    }

    /// How many writes this driver has made to `EXTCNF_CTRL`.
    pub fn extcnf_writes(&self) -> u32 {
        self.0.borrow().phy.extcnf_writes
    }

    /// The part lets this driver's software bit go on its own, as a reset
    /// does.
    pub fn software_flag_let_go(&self) {
        let mut model = self.0.borrow_mut();
        model.phy.sw_requested = false;
        model.refresh_ownership();
    }

    /// Moving the MAC onto SMBus starts §8.2.3's transition, and it never
    /// ends.
    pub fn transition_sticks_once_the_mac_is_on_smbus(&self) {
        self.0.borrow_mut().phy.transition_sticks_on_smbus = true;
    }

    /// The engine holds the interface from now until the modelled clock
    /// reads `nanos`.
    pub fn engine_holds_the_interface_until(&self, nanos: u64) {
        let mut model = self.0.borrow_mut();
        model.phy.mdio_sticks = true;
        model.phy.engine_lets_go_at = Some(nanos);
    }

    /// Where the PHY stands with respect to the MAC.
    pub fn lcd(&self) -> Lcd {
        self.0.borrow().lcd
    }

    /// How many times `LANPHYPC` was cycled.
    pub fn power_cycles(&self) -> u32 {
        self.0.borrow().power_cycles
    }

    /// §9.5.3.2's Port General Configuration as the PHY holds it now.
    pub fn phy_port_general(&self) -> u16 {
        self.0.borrow().phy.port_general
    }

    /// How many resets carried `PHY_RST`.
    pub fn phy_resets(&self) -> u32 {
        self.0.borrow().phy_resets
    }

    /// Every register offset the driver has written.
    pub fn written(&self) -> BTreeSet<usize> {
        self.0.borrow().written.clone()
    }

    /// Nothing in the window decodes `reg`, so reads of it answer ones.
    pub fn window_does_not_decode(&self, reg: usize) {
        self.0.borrow_mut().not_decoding = Some(reg);
    }

    /// §10.2.2.7's `Error`: this part cannot complete a read of one register.
    pub fn mdi_fails_read_of(&self, addr: u8, reg: u8) {
        self.0.borrow_mut().phy.failing = Some((addr, reg));
    }

    /// Whatever answers the MDI transaction is not the PHY §9.5.2.3 describes.
    pub fn phy_identifies_as(&self, high: u16) {
        self.0.borrow_mut().phy.file[reg::IDENTIFIER_HIGH as usize] = high;
    }

    /// Nothing drives one of §9.3's two PHY addresses, which is the reading of
    /// that section under which the registers are at the other one.
    pub fn phy_is_deaf_at(&self, addr: u8) {
        self.0.borrow_mut().phy.deaf = Some(addr);
    }

    /// §9.5.2.1's Reset, written to the Control register by an agent that is not
    /// this driver — the same write this model takes from a driver that carried
    /// the bit into a register it composed.
    pub fn phy_is_reset(&self) {
        let mut model = self.0.borrow_mut();
        let data = carried::CONTROL.default | control::RESET;
        model.phy_write(phy::SPECIFIC, reg::CONTROL, data);
    }

    /// One PHY register, read without any of its side effects — for an
    /// assertion about what §9.5's tables leave in it.
    pub fn phy_peek(&self, addr: u8, register: u8) -> u16 {
        self.0.borrow_mut().phy_read(addr, register)
    }

    /// The restart of auto-negotiation resolves. **A wire event and not a
    /// register**: §9.5.2.1's restart takes a 1000BASE-T link seconds, so the
    /// bring-up that wrote it reads §9.5.2.2 long before this, and nothing the
    /// driver does shortens it.
    pub fn negotiation_settles(&self) {
        let mut model = self.0.borrow_mut();
        model.phy.negotiating = false;
        model.refresh_phy_link();
    }

    /// What the partner on the wire offers — §9.5.2.5's advertisement and
    /// §9.5.2.10's — which is the other half of every resolution.
    pub fn partner_advertises(&self, abilities: u16, gigabit: u16) {
        self.0.borrow_mut().partner = (abilities, gigabit);
    }

    /// The four the driver takes.
    pub fn parts(&self) -> (Bar, Ticker, Grant, Line) {
        (
            Bar(Rc::clone(&self.0)),
            Ticker(Rc::clone(&self.0)),
            Grant(Rc::clone(&self.0)),
            Line(Rc::clone(&self.0)),
        )
    }

    /// This part does not take a write to `reg`, whatever is written. A fault
    /// injector, not a clause: it is how a test reaches
    /// [`crate::Refusal::NotAccepted`].
    pub fn refuses_writes_to(&self, reg: usize) {
        self.0.borrow_mut().refusing = Some(reg);
    }

    /// `CTRL.RST` on this part never clears itself, against §10.2.2.1. A fault
    /// injector too: it is how a test reaches
    /// [`crate::Refusal::ResetUnfinished`].
    pub fn reset_never_clears(&self) {
        self.0.borrow_mut().reset_sticks = true;
    }

    /// The claim stops answering its interrupt record — what the kernel does
    /// once the function is no longer this process's.
    pub fn claim_taken_away(&self) {
        self.0.borrow_mut().claim_gone = true;
    }

    /// Set an interrupt cause, as §10.2.4.4's `ICS` does, without a frame or a
    /// link change behind it.
    pub fn cause(&self, causes: u32) {
        self.0.borrow_mut().raise(causes);
    }

    /// This part has no NVM, so §10.2.5.23's "if no NVM is present" arm is
    /// what a driver reading `RAH0` finds.
    pub fn without_nvm(&self) {
        let mut model = self.0.borrow_mut();
        model.has_nvm = false;
        model.load_station_address();
    }

    /// The link came up or went away. §10.2.4.1: `LSC` "is set whenever the
    /// link status changes (either from up to down, or from down to up)".
    /// A partner appears on the wire, or goes away.
    ///
    /// **On the part with a PHY behind `MDIC` this is the wire and not the
    /// link**: a partner on a PHY still powered down, isolated or negotiating
    /// over abilities nothing restarted raises nothing at all, which is
    /// [`Model::phy_unconfigured`]'s whole subject.
    pub fn set_link(&self, up: bool) {
        let mut model = self.0.borrow_mut();
        if model.link_up == up {
            return;
        }
        model.link_up = up;
        match model.part {
            Part::I219 => model.refresh_phy_link(),
            Part::E82574 => {
                model.refresh_status();
                model.raise(cause::LSC);
            }
        }
    }

    /// A frame arrives on the wire. It is placed when the device next runs.
    pub fn deliver(&self, frame: &[u8]) {
        self.0.borrow_mut().inbound.push_back(frame.to_vec());
    }

    /// Let the device work: place what has arrived, send what is published,
    /// write back what it holds.
    pub fn run(&self) {
        self.0.borrow_mut().run();
    }

    /// Frames the device has put on the wire, taken.
    pub fn sent(&self) -> Vec<Vec<u8>> {
        core::mem::take(&mut self.0.borrow_mut().sent)
    }

    /// Stop the device acting on a tail register write, so it acts only when
    /// [`Self::run`] says so.
    pub fn hold(&self) {
        self.0.borrow_mut().held = true;
    }

    /// One register, read without any of its side effects — for an assertion
    /// about what the driver programmed.
    pub fn peek(&self, reg: usize) -> u32 {
        self.0.borrow().get(reg)
    }

    /// Write one receive descriptor's status word behind the driver's back, so
    /// a test can hand it a number no working part would write.
    pub fn poke_rx_status(&self, index: usize, word: u64) {
        let mut model = self.0.borrow_mut();
        let at = model.rx_ring() + index * rx_desc::BYTES + 8;
        model.desc_write(at, word);
    }

    /// Null the data address of one receive descriptor, which is the state
    /// §7.1.7.2's null descriptor padding is defined over.
    pub fn null_rx_buffer(&self, index: usize) {
        let mut model = self.0.borrow_mut();
        let at = model.rx_ring() + index * rx_desc::BYTES;
        model.desc_write(at, 0);
    }

    /// §7.4.5's spurious interrupt: a message whose cause has already been
    /// cleared, so `ICR` says nothing at all when the driver reads it.
    pub fn spurious(&self) {
        let mut model = self.0.borrow_mut();
        model.messages = model.messages.saturating_add(1);
    }

    /// Bytes of the grant, for an assertion about a frame's content.
    pub fn bytes(&self, at: usize, len: usize) -> Vec<u8> {
        self.0.borrow().memory[at..at + len].to_vec()
    }

    /// Put bytes in the grant, the way the driver's caller fills a transmit
    /// buffer.
    pub fn put_bytes(&self, at: usize, bytes: &[u8]) {
        self.0.borrow_mut().memory[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// Say what happened, with the seed that reproduces it.
    pub fn because(&self, what: &str) -> std::string::String {
        format!("seed {}: {what}", self.seed())
    }
}

pub struct Bar(Rc<RefCell<Model>>);

impl Registers for Bar {
    fn bytes(&self) -> usize {
        regs::REGISTER_BYTES
    }

    fn read(&self, reg: usize) -> u32 {
        self.0.borrow_mut().read(reg)
    }

    fn write(&self, reg: usize, value: u32) {
        self.0.borrow_mut().write(reg, value);
    }
}

pub struct Ticker(Rc<RefCell<Model>>);

impl Clock for Ticker {
    fn nanos(&self) -> u64 {
        let mut model = self.0.borrow_mut();
        model.nanos += CLOCK_STEP_NANOS;
        model.nanos
    }

    /// The clock moves on with nothing reaching the part, which is the whole of
    /// what a caller giving the processor away looks like from here.
    ///
    /// **Half the seeds get a pause that lands short.** A substrate's pause is
    /// not a bound, and a driver that took one for a bound would be waiting on
    /// a promise nobody made — so on those seeds every pace and every deadline
    /// has to come back to the clock to be kept.
    fn pause(&self, nanos: u64) {
        let mut model = self.0.borrow_mut();
        let short = model.seed.is_multiple_of(2);
        model.nanos += if short { nanos - nanos / 4 } else { nanos };
    }
}

pub struct Grant(Rc<RefCell<Model>>);

impl DmaBuffers for Grant {
    fn bytes(&self) -> usize {
        self.0.borrow().memory.len()
    }

    fn device_addr(&self, at: usize) -> u64 {
        DEVICE_BASE + at as u64
    }

    fn read(&self, at: usize) -> u64 {
        self.0.borrow().desc_read(at)
    }

    fn write(&self, at: usize, word: u64) {
        self.0.borrow_mut().desc_write(at, word);
    }

    // The host has one memory and one observer of it, so the two barriers are
    // where they are on a machine — at the boundary — and cost nothing here.
    fn publish(&self) {}
    fn observe(&self) {}
}

pub struct Line(Rc<RefCell<Model>>);

/// What the modelled claim answers when it has no count to give.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unanswered {
    /// Nothing since the last read — the kernel's `WouldBlock`.
    Idle,
    /// The function is no longer this driver's.
    Gone,
}

impl Interrupts for Line {
    type Refused = Unanswered;
    const IDLE: Unanswered = Unanswered::Idle;

    fn taken(&self) -> Result<u32, Unanswered> {
        let mut model = self.0.borrow_mut();
        if model.claim_gone {
            return Err(Unanswered::Gone);
        }
        match core::mem::take(&mut model.messages) {
            0 => Err(Unanswered::Idle),
            count => Ok(count),
        }
    }
}
