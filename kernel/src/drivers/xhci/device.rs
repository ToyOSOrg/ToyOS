use core::num::NonZeroU8;

use crate::log;
use toyos_xhci::enumerate::{
    self, ep0_packet_from_descriptor, initial_ep0_packet, Act, Enumeration, Learnt, Next, Request,
};
use toyos_xhci::job::{Await, Outcome, Stages};
use toyos_xhci::port::{self, Reset};
use toyos_xhci::recovery;
use super::{deadline, Answer, Trb, TrbRing, What, XhciController, PAGE};
use super::{OFF_INPUT_CTX, OFF_DATA_BUF};
use super::{DEV_INT_RING, DEV_EP0_RING, DEV_OUT_CTX, DEV_REPORT, EP0_DCI};
use super::{TRB_ENABLE_SLOT, TRB_ADDRESS_DEVICE, TRB_CONFIGURE_EP, TRB_EVALUATE_CONTEXT};
use super::{enqueue_control, CC_SUCCESS};

use super::hid::{HidType, HidRole, HidDevice};
use super::msc::{MscInterface, MscRings};

// `wTotalLength` is clamped to this size; the scratch page is four times it.
const MAX_CONFIG_DESC: usize = 256;

/// One endpoint this driver can configure; only [`Endpoint::new`] can build one, keeping `dci` valid.
#[derive(Clone, Copy)]
pub(super) struct Endpoint {
    pub(super) addr: u8,
    // 2..=31 by construction; `bind` and `write_ctx32` use it unchecked.
    dci: u8,
    pub(super) max_packet: u16,
    /// The SuperSpeed companion's burst size; zero means one packet per burst.
    pub(super) max_burst: u8,
    /// bInterval. Only an interrupt endpoint uses it.
    pub(super) interval: u8,
}

impl Endpoint {
    pub(super) fn dci(&self) -> u8 {
        self.dci
    }

    // `None` for endpoint 0 (`0x80`/`0x10`), whose DCI would name the slot or EP0 context.
    fn new(addr: u8, max_packet: u16, interval: u8) -> Option<Self> {
        let num = addr & 0x0F;
        (num != 0).then(|| Self {
            addr,
            dci: num * 2 + u8::from(addr & 0x80 != 0),
            max_packet,
            max_burst: 0,
            interval,
        })
    }
}

#[derive(Clone, Copy)]
struct HidInterfaceInfo {
    protocol: HidType,
    iface_num: u8,
    ep: Endpoint,
}

/// Always complete: no variant holds an unresolved endpoint; [`Walk`] accumulates until conversion.
#[derive(Clone, Copy)]
enum Function {
    Hid(HidInterfaceInfo),
    Msc(MscInterface),
}

impl Function {
    // Only whether a boot protocol exists to select is given onward, not endpoints or interface number.
    fn shape(self) -> enumerate::Function {
        match self {
            // A tablet has no boot protocol to select; asking is a request it may stall for.
            Self::Hid(info) if info.protocol == HidType::Tablet => enumerate::Function::Hid,
            Self::Hid(_) => enumerate::Function::BootHid,
            Self::Msc(_) => enumerate::Function::Msc,
        }
    }
}

/// Same interface mid-walk; [`Walk::finish`] is the only completeness test into [`Function`].
enum Walk {
    Hid { protocol: HidType, iface_num: u8, ep: Option<Endpoint> },
    Msc { iface_num: u8, in_ep: Option<Endpoint>, out_ep: Option<Endpoint> },
}

impl Walk {
    // Moves a finished interface into the running answer if this driver can bind it.
    fn finish(self, hid: &mut Option<HidInterfaceInfo>, msc: &mut Option<MscInterface>) {
        match self {
            Self::Hid { protocol, iface_num, ep: Some(ep) } => {
                if hid.is_none() {
                    *hid = Some(HidInterfaceInfo { protocol, iface_num, ep });
                }
            }
            Self::Hid { .. } => {}
            Self::Msc { iface_num, in_ep: Some(in_ep), out_ep: Some(out_ep) } => {
                if msc.is_none() {
                    *msc = Some(MscInterface { iface_num, in_ep, out_ep });
                }
            }
            Self::Msc { iface_num, .. } => {
                log!("xHCI: mass-storage interface {iface_num} has no pair of bulk endpoints \
                     this driver can configure, skipping");
            }
        }
    }
}

// Evaluate Context, not Configure Endpoint: §4.6.7 changes only this field, via the A1 flag.
fn evaluate_ep0_trb(ctrl: &mut XhciController, slot_id: u8, max_packet: u16) -> Trb {
    let dma = ctrl.dma();
    let input_ctx = super::zero_dma(dma, OFF_INPUT_CTX, PAGE);
    ctrl.write_ctx32(input_ctx, 0, 1, 1 << 1);
    let ep0_dw1 = (3u32 << 1) | (4u32 << 3) | ((max_packet as u32) << 16);
    ctrl.write_ctx32(input_ctx, 2, 1, ep0_dw1);

    let mut evaluate = Trb::ZERO;
    evaluate.param = input_ctx.device_addr();
    evaluate.control = TRB_EVALUATE_CONTEXT | ((slot_id as u32) << 24);
    evaluate
}

// Little-endian 16-bit field at `at`, or 0 past the end; descriptor fields are not aligned.
fn le16(buf: &[u8], at: usize) -> u16 {
    let lo = buf.get(at).copied().unwrap_or(0) as u16;
    let hi = buf.get(at + 1).copied().unwrap_or(0) as u16;
    lo | (hi << 8)
}

// Bounded by `buf`: every length is device-supplied, and a zero length ends the walk.
fn parse_config(buf: &[u8]) -> Option<(u8, Function)> {
    let total_len = (le16(buf, 2) as usize).min(buf.len());
    let config_val = *buf.get(5)?;

    let mut hid: Option<HidInterfaceInfo> = None;
    let mut msc: Option<MscInterface> = None;
    // Which interface the endpoint descriptors that follow belong to.
    let mut current: Option<Walk> = None;
    // A SuperSpeed companion describes the endpoint immediately before it.
    let mut last_ep_in: Option<bool> = None;

    let mut offset = 0usize;
    while offset + 2 <= total_len {
        let desc_len = buf[offset] as usize;
        let desc_type = buf[offset + 1];
        if desc_len == 0 {
            break;
        }
        let desc = match buf.get(offset..(offset + desc_len).min(total_len)) {
            Some(d) => d,
            None => break,
        };

        match desc_type {
            // Interface
            4 if desc.len() >= 9 => {
                if let Some(done) = current.take() {
                    done.finish(&mut hid, &mut msc);
                }
                let (class, sub, proto) = (desc[5], desc[6], desc[7]);
                current = if class == 0x08 && sub == 0x06 && proto == 0x50 {
                    Some(Walk::Msc { iface_num: desc[2], in_ep: None, out_ep: None })
                } else if class == 3 {
                    let protocol = match (sub, proto) {
                        (1, 1) => Some(HidType::Keyboard),
                        (1, 2) => Some(HidType::Mouse),
                        (0, _) => Some(HidType::Tablet),
                        _ => None,
                    };
                    protocol.map(|protocol| Walk::Hid { protocol, iface_num: desc[2], ep: None })
                } else {
                    None
                };
            }
            // Endpoint
            5 if desc.len() >= 7 => {
                let transfer = desc[3] & 0x3;
                last_ep_in = None;
                // `if let`, not `let ... else`: the offset advance runs regardless, at the loop's bottom.
                if let Some(ep) = Endpoint::new(desc[2], le16(desc, 4), desc[6]) {
                    let is_in = ep.addr & 0x80 != 0;
                    match &mut current {
                        Some(Walk::Hid { ep: slot, .. }) if is_in && slot.is_none() => {
                            *slot = Some(ep);
                        }
                        // Bulk only: an MSC interrupt endpoint belongs to CBI, unsupported here.
                        Some(Walk::Msc { in_ep, out_ep, .. }) if transfer == 2 => {
                            if is_in && in_ep.is_none() {
                                *in_ep = Some(ep);
                                last_ep_in = Some(true);
                            } else if !is_in && out_ep.is_none() {
                                *out_ep = Some(ep);
                                last_ep_in = Some(false);
                            }
                        }
                        _ => {}
                    }
                }
            }
            // SuperSpeed Endpoint Companion: states the burst size of the endpoint above it.
            0x30 if desc.len() >= 3 => {
                if let (Some(Walk::Msc { in_ep, out_ep, .. }), Some(is_in)) =
                    (&mut current, last_ep_in)
                {
                    if let Some(ep) = if is_in { in_ep } else { out_ep } {
                        ep.max_burst = desc[2];
                    }
                }
            }
            _ => {}
        }
        offset += desc_len;
    }
    if let Some(done) = current {
        done.finish(&mut hid, &mut msc);
    }

    // MSC wins a tie with HID: a device offering a disk is a disk; `Walk::finish` already guarantees completeness here.
    if let Some(m) = msc {
        return Some((config_val, Function::Msc(m)));
    }
    Some((config_val, Function::Hid(hid?)))
}

/// Asks a port to reset, split from waiting for it because the runtime path must not block a scheduler pass across it.
pub fn reset_port(ctrl: &mut XhciController, port_idx: u8, kind: Reset) {
    let portsc = ctrl.read_portsc(port_idx);
    ctrl.write_portsc(port_idx, port::reset_write(kind, portsc));
}

/// Whether the port has finished the reset it was asked for.
pub fn reset_done(ctrl: &XhciController, port_idx: u8) -> bool {
    super::port_answers() && ctrl.read_portsc(port_idx).reset_changed()
}

/// One device's enumeration: the state an answer needs, carried because the pass that asked gave up its stack.
pub(super) struct Enumerating {
    port_idx: u8,
    speed: u8,
    // The enabled slot; every refusal from here carries it back, since only Disable Slot returns one.
    slot_id: u8,
    block: usize,
    ep0_ring: TrbRing,
    /// EP0's Max Packet Size as the controller currently believes it.
    packet: u16,
    seq: Enumeration,
    // What an answer means is read against this; `perform` must set it before every submit.
    issued: Act,
    /// The configuration value and the function the descriptor named.
    parsed: Option<(u8, Function)>,
    // Carried, not rebuilt: a second `TrbRing::init` would zero memory the controller is already reading.
    rings: Option<Rings>,
}

/// The transfer rings a device's Configure Endpoint put into the Running state.
#[derive(Clone, Copy)]
enum Rings {
    Hid(TrbRing),
    Msc(MscRings),
}


/// Acknowledges the reset, reads what the port came up as, and asks for a slot — the only act with no device state to carry.
///
/// `after` is the reset this enumeration follows, and what the acknowledge is a
/// function of; `port::enumeration_ack` is that function, and the simulator's
/// enumerate step calls the same one.
pub(super) fn begin(ctrl: &mut XhciController, port_idx: u8, after: Option<Reset>) {
    let portsc = ctrl.read_portsc(port_idx);
    ctrl.write_portsc(port_idx, port::enumeration_ack(after, portsc));

    let portsc = ctrl.read_portsc(port_idx);
    // A stale-word guard, not the reset's verdict — `port::reset_outcome` decided that.
    if !portsc.enabled() {
        log!("xHCI: port {} reset but not enabled (PORTSC {:#010x}); skipping it",
            port_idx + 1, portsc.raw());
        return finish(ctrl, port_idx, None);
    }
    let speed = portsc.speed();
    log!("xHCI: port {} enabled, speed={}", port_idx + 1, speed);
    let Some(packet) = initial_ep0_packet(speed) else {
        log!("xHCI: port {} came up at speed {speed}, which is not a speed this driver has a \
             control-endpoint packet size for; skipping it", port_idx + 1);
        return finish(ctrl, port_idx, None);
    };

    let (seq, _) = Enumeration::begin();
    let mut enable_slot = Trb::ZERO;
    enable_slot.control = TRB_ENABLE_SLOT;
    let on = Await::Command { trb: ctrl.submit_command(enable_slot) };
    let what = What::SlotWanted { port_idx, speed, packet, seq };
    ctrl.outstanding.submit(what, on, Stages::One, deadline());
}

/// Called once Enable Slot answers; everything after this has a device to write for.
pub(super) fn slot_answered(
    ctrl: &mut XhciController,
    port_idx: u8,
    speed: u8,
    packet: u16,
    seq: Enumeration,
    outcome: Outcome,
) {
    let Outcome::Command { code: CC_SUCCESS, slot: slot_id } = outcome else {
        log!("xHCI: Enable Slot on port {}: {}", port_idx + 1, Answer(outcome));
        return finish(ctrl, port_idx, None);
    };
    // A slot id can exceed the pool's device blocks: CONFIG's MaxSlotsEn is only advisory and QEMU ignores it.
    // Nothing of the driver's is written yet; refusal below still carries the slot id back.
    let Some(block) = ctrl.layout.device(slot_id) else {
        log!("xHCI: slot {} is beyond the pool's {} device blocks, dropping port {}",
            slot_id, ctrl.layout.dev_blocks, port_idx + 1);
        return refuse(ctrl, port_idx, slot_id);
    };
    log!("xHCI: slot {} enabled (dma +{:#x})", slot_id, block);

    let ep0_ring = TrbRing::init(ctrl.dma().subview(block + DEV_EP0_RING, PAGE));
    let state = Enumerating {
        port_idx,
        speed,
        slot_id,
        block,
        ep0_ring,
        packet,
        seq,
        issued: Act::Command(enumerate::Command::EnableSlot),
        parsed: None,
        rings: None,
    };
    advance(ctrl, state, Learnt::Nothing);
}

/// Reads the outstanding act's answer and either continues the sequence or leaves the port with what it spent.
pub(super) fn stepped(ctrl: &mut XhciController, mut state: Enumerating, outcome: Outcome) {
    let port = state.port_idx + 1;
    let learnt = match state.issued {
        Act::Command(cmd) => {
            if !outcome.succeeded() {
                log!("xHCI: {} on port {port}: {}", command_name(cmd), Answer(outcome));
                return refuse(ctrl, state.port_idx, state.slot_id);
            }
            match cmd {
                enumerate::Command::AddressDevice => log!("xHCI: device addressed"),
                enumerate::Command::ConfigureEndpoint => log!("xHCI: endpoint configured"),
                enumerate::Command::SetEp0Dequeue => {
                    log!("xHCI: EP0 on port {port} runs again after the stall")
                }
                enumerate::Command::EnableSlot
                | enumerate::Command::EvaluateEp0
                | enumerate::Command::ResetEp0 => {}
            }
            Learnt::Nothing
        }
        // SET_PROTOCOL is the one request a device may refuse without being refused for it: it still reports in the format its descriptors promised.
        Act::Request(Request::SetProtocol) => {
            if !outcome.succeeded() {
                log!("xHCI: SET_PROTOCOL on port {port}: {}", Answer(outcome));
                // Going on is the decision, but EP0 stays halted until recovery clears it (USB 2.0 §8.5.3.4).
                Learnt::Stalled
            } else {
                Learnt::Nothing
            }
        }
        Act::Request(request) => {
            let want = match request {
                Request::DeviceDescriptor { want } => want,
                Request::ConfigDescriptor => MAX_CONFIG_DESC as u16,
                Request::SetConfiguration | Request::SetProtocol => 0,
            };
            let Some(delivered) = delivered(outcome, want) else {
                log!("xHCI: {} on port {port}: {}", request_name(request), Answer(outcome));
                return refuse(ctrl, state.port_idx, state.slot_id);
            };
            match read_back(ctrl, &mut state, request, delivered) {
                Ok(learnt) => learnt,
                Err(()) => return refuse(ctrl, state.port_idx, state.slot_id),
            }
        }
    };
    advance(ctrl, state, learnt);
}

/// Ask the sequence what is owed next and do it.
fn advance(ctrl: &mut XhciController, state: Enumerating, learnt: Learnt) {
    match state.seq.completed(learnt) {
        Next::Act(seq, act) => perform(ctrl, Enumerating { seq, ..state }, act),
        Next::Bind => bind(ctrl, state),
        Next::Refuse => {
            log!("xHCI: no HID boot interface found on port {}, skipping it",
                state.port_idx + 1);
            refuse(ctrl, state.port_idx, state.slot_id)
        }
    }
}

/// Submit one act and record that the controller owes an answer for it.
fn perform(ctrl: &mut XhciController, mut state: Enumerating, act: Act) {
    state.issued = act;
    let submitted = match act {
        Act::Command(cmd) => command(ctrl, &mut state, cmd)
            .map(|trb| (Await::Command { trb: ctrl.submit_command(trb) }, Stages::One)),
        Act::Request(request) => Some(control(ctrl, &mut state, request)),
    };
    let Some((on, stages)) = submitted else {
        return refuse(ctrl, state.port_idx, state.slot_id);
    };
    ctrl.outstanding.submit(What::Enumerating(state), on, stages, deadline());
}

/// The TRB for one command act, or `None` where the driver has already refused it.
fn command(
    ctrl: &mut XhciController,
    state: &mut Enumerating,
    cmd: enumerate::Command,
) -> Option<Trb> {
    match cmd {
        // Answered in `begin`; never asked for again.
        enumerate::Command::EnableSlot => None,
        enumerate::Command::AddressDevice => Some(address_device_trb(ctrl, state)),
        enumerate::Command::EvaluateEp0 => {
            Some(evaluate_ep0_trb(ctrl, state.slot_id, state.packet))
        }
        enumerate::Command::ConfigureEndpoint => configure_endpoint_trb(ctrl, state),
        // EP0's own stall recovery, via the same `recovery_trb` builder bulk/interrupt endpoints use; its Set TR Dequeue re-points the ring off the stalled TRB.
        enumerate::Command::ResetEp0 => Some(ctrl.recovery_trb(
            recovery::Command::ResetEndpoint,
            state.slot_id,
            EP0_DCI,
            &mut state.ep0_ring,
            state.block + DEV_EP0_RING,
        )),
        enumerate::Command::SetEp0Dequeue => Some(ctrl.recovery_trb(
            recovery::Command::SetDequeue,
            state.slot_id,
            EP0_DCI,
            &mut state.ep0_ring,
            state.block + DEV_EP0_RING,
        )),
    }
}

// A request with a data stage is two completions; `enqueue_control` already accounts for both.
fn control(
    ctrl: &mut XhciController,
    state: &mut Enumerating,
    request: Request,
) -> (Await, Stages) {
    let dma = ctrl.dma();
    let scratch = dma.device_addr() + OFF_DATA_BUF as u64;
    let (bm_request_type, b_request, w_value, w_index, data, len) = match request {
        Request::DeviceDescriptor { want } => (0x80, 0x06, 0x0100, 0, Some(scratch), want),
        Request::ConfigDescriptor => {
            (0x80, 0x06, 0x0200, 0, Some(scratch), MAX_CONFIG_DESC as u16)
        }
        Request::SetConfiguration => {
            let (config_val, _) = state.parsed.expect("a configuration named a function");
            (0x00, 0x09, config_val as u16, 0, None, 0)
        }
        Request::SetProtocol => {
            let iface = match state.parsed {
                Some((_, Function::Hid(info))) => info.iface_num,
                _ => unreachable!("only a HID interface has a boot protocol to select"),
            };
            (0x21, 0x0B, 0, iface as u16, None, 0)
        }
    };
    // The scratch page is shared and enumeration is serial; zeroed first so a short answer can't leak stale data.
    if data.is_some() {
        super::zero_dma(dma, OFF_DATA_BUF, MAX_CONFIG_DESC);
    }
    let trbs = enqueue_control(
        &mut state.ep0_ring, bm_request_type, b_request, w_value, w_index, data, len,
    );
    ctrl.ring_doorbell(state.slot_id, 1);
    trbs.awaits(state.slot_id)
}

/// What a completed control request left in the scratch page; `Err` is a device already refused.
fn read_back(
    ctrl: &mut XhciController,
    state: &mut Enumerating,
    request: Request,
    delivered: u16,
) -> Result<Learnt, ()> {
    let port = state.port_idx + 1;
    // A bounded copy, not a borrow: every read below indexes this slice, so a device-chosen length can't walk past it.
    // Safe to copy now: `control` zeroed this page before the transfer, and `read_back` runs only once that transfer completes.
    let mut scratch = [0u8; MAX_CONFIG_DESC];
    ctrl.dma().copy_to(OFF_DATA_BUF, &mut scratch);
    let scratch: &[u8] = &scratch;
    match request {
        Request::DeviceDescriptor { want: 8 } => {
            if delivered < 8 {
                log!("xHCI: port {port} would not give up the first 8 bytes of its device \
                     descriptor: {delivered} B delivered");
                return Err(());
            }
            let stated = scratch[7];
            let Some(ep0_packet) = ep0_packet_from_descriptor(state.speed, stated) else {
                log!("xHCI: port {port} states bMaxPacketSize0={stated}, which is not a control \
                     packet size a speed-{} device has; skipping it", state.speed);
                return Err(());
            };
            if ep0_packet == state.packet {
                return Ok(Learnt::Nothing);
            }
            log!("xHCI: port {port} EP0 packet size {} -> {ep0_packet}", state.packet);
            state.packet = ep0_packet;
            Ok(Learnt::Ep0PacketWrong)
        }
        Request::DeviceDescriptor { .. } => {
            if delivered < 18 {
                log!("xHCI: GET_DESCRIPTOR(Device) on port {port}: {delivered} B delivered");
                return Err(());
            }
            let descriptor = &scratch[..18];
            log!("xHCI: device class={:#x} vendor={:04x} product={:04x}",
                descriptor[4], le16(descriptor, 8), le16(descriptor, 10));
            Ok(Learnt::Nothing)
        }
        // Nine bytes is the header holding `wTotalLength`; the parser is bounded by what arrived, not what was asked.
        Request::ConfigDescriptor => {
            if delivered < 9 {
                log!("xHCI: port {port} answered {delivered} B to GET_DESCRIPTOR(Config); a \
                     configuration descriptor is at least 9", );
                return Err(());
            }
            // `delivered` bounds the parse; the `min` refuses a device claiming more than the page holds.
            let config = &scratch[..(delivered as usize).min(scratch.len())];
            let Some((config_val, function)) = parse_config(config) else {
                return Ok(Learnt::Nothing);
            };
            match function {
                Function::Msc(msc) => log!("xHCI: mass storage iface={} in={:#x}/{} out={:#x}/{}",
                    msc.iface_num, msc.in_ep.addr, msc.in_ep.max_packet,
                    msc.out_ep.addr, msc.out_ep.max_packet),
                Function::Hid(info) => log!("xHCI: HID {} iface={} ep={:#x} max_pkt={} \
                     interval={} dci={}", hid_kind(info.protocol), info.iface_num, info.ep.addr,
                    info.ep.max_packet, info.ep.interval, info.ep.dci()),
            }
            state.parsed = Some((config_val, function));
            Ok(Learnt::Function(function.shape()))
        }
        Request::SetConfiguration => {
            log!("xHCI: configuration set");
            Ok(Learnt::Nothing)
        }
        Request::SetProtocol => Ok(Learnt::Nothing),
    }
}

/// Bytes a control request actually moved, or `None` if it did not complete; Success alone doesn't say.
fn delivered(outcome: Outcome, want: u16) -> Option<u16> {
    let Outcome::Transfer { code: CC_SUCCESS, residue } = outcome else { return None };
    // A residue past `want` is the controller contradicting itself; `min` refuses to believe it.
    Some(want.saturating_sub(residue.min(u16::MAX as u32) as u16))
}

fn command_name(cmd: enumerate::Command) -> &'static str {
    match cmd {
        enumerate::Command::EnableSlot => "Enable Slot",
        enumerate::Command::AddressDevice => "Address Device",
        enumerate::Command::EvaluateEp0 => "Evaluate Context (EP0 packet size)",
        enumerate::Command::ConfigureEndpoint => "Configure Endpoint",
        enumerate::Command::ResetEp0 => "Reset Endpoint (EP0)",
        enumerate::Command::SetEp0Dequeue => "Set TR Dequeue Pointer (EP0)",
    }
}

fn request_name(request: Request) -> &'static str {
    match request {
        Request::DeviceDescriptor { want: 8 } => "GET_DESCRIPTOR(Device, 8)",
        Request::DeviceDescriptor { .. } => "GET_DESCRIPTOR(Device)",
        Request::ConfigDescriptor => "GET_DESCRIPTOR(Config)",
        Request::SetConfiguration => "SET_CONFIGURATION",
        Request::SetProtocol => "SET_PROTOCOL",
    }
}

fn hid_kind(protocol: HidType) -> &'static str {
    match protocol {
        HidType::Keyboard => "keyboard",
        HidType::Mouse => "mouse",
        HidType::Tablet => "tablet",
    }
}

/// The Address Device command, with the slot and control endpoint this device will be known by.
fn address_device_trb(ctrl: &mut XhciController, state: &Enumerating) -> Trb {
    let dma = ctrl.dma();
    let input_ctx = super::zero_dma(dma, OFF_INPUT_CTX, PAGE);

    ctrl.write_ctx32(input_ctx, 0, 1, 0x3); // Add Slot + EP0
    let slot_dw0 = ((state.speed as u32) << 20) | (1u32 << 27);
    ctrl.write_ctx32(input_ctx, 1, 0, slot_dw0);
    ctrl.write_ctx32(input_ctx, 1, 1, (state.port_idx as u32 + 1) << 16);

    let ep0_dw1 = (3u32 << 1) | (4u32 << 3) | ((state.packet as u32) << 16);
    ctrl.write_ctx32(input_ctx, 2, 1, ep0_dw1);
    let ep0_dequeue = state.ep0_ring.dequeue();
    ctrl.write_ctx32(input_ctx, 2, 2, ep0_dequeue as u32);
    ctrl.write_ctx32(input_ctx, 2, 3, (ep0_dequeue >> 32) as u32);
    ctrl.write_ctx32(input_ctx, 2, 4, 8);

    let out_ctx = super::zero_dma(dma, state.block + DEV_OUT_CTX, PAGE / 2);
    ctrl.write_dcbaa(state.slot_id as usize, out_ctx.device_addr());

    let mut addr_dev = Trb::ZERO;
    addr_dev.param = input_ctx.device_addr();
    addr_dev.control = TRB_ADDRESS_DEVICE | ((state.slot_id as u32) << 24);
    addr_dev
}

/// The Configure Endpoint command, with its transfer rings recorded; `None` is a refusal — a disk past the pool's mass-storage blocks — not a failure.
fn configure_endpoint_trb(ctrl: &mut XhciController, state: &mut Enumerating) -> Option<Trb> {
    let (_, function) = state.parsed.expect("a configuration named a function");
    let rings = match function {
        Function::Hid(info) => Rings::Hid(hid_input_context(ctrl, state, &info)),
        Function::Msc(info) => Rings::Msc(super::msc::prepare(
            ctrl, state.slot_id, state.speed, state.port_idx, &info,
        )?),
    };
    state.rings = Some(rings);

    let mut configure = Trb::ZERO;
    configure.param = ctrl.dma().subview(OFF_INPUT_CTX, PAGE).device_addr();
    configure.control = TRB_CONFIGURE_EP | ((state.slot_id as u32) << 24);
    Some(configure)
}

/// The input context for one HID interrupt endpoint, and the ring it runs on.
fn hid_input_context(
    ctrl: &mut XhciController,
    state: &Enumerating,
    info: &HidInterfaceInfo,
) -> TrbRing {
    let dma = ctrl.dma();
    let int_ep_dci = info.ep.dci();
    let int_ring = TrbRing::init(dma.subview(state.block + DEV_INT_RING, PAGE));

    let input_ctx = super::zero_dma(dma, OFF_INPUT_CTX, PAGE);

    ctrl.write_ctx32(input_ctx, 0, 1, (1u32 << (int_ep_dci as u32)) | 1);

    let slot_dw0 = ((state.speed as u32) << 20) | ((int_ep_dci as u32) << 27);
    ctrl.write_ctx32(input_ctx, 1, 0, slot_dw0);
    ctrl.write_ctx32(input_ctx, 1, 1, (state.port_idx as u32 + 1) << 16);

    let ep_ctx_index = int_ep_dci as usize + 1;
    let interval_val = if info.ep.interval == 0 { 0u32 } else if state.speed <= 2 {
        let frames = (info.ep.interval as u32) * 8;
        let mut exp = 0u32;
        let mut v = frames;
        while v > 1 { v >>= 1; exp += 1; }
        exp
    } else {
        (info.ep.interval - 1) as u32
    };
    ctrl.write_ctx32(input_ctx, ep_ctx_index, 0, interval_val << 16);

    let ep_dw1 = (3u32 << 1) | (7u32 << 3) | ((info.ep.max_packet as u32) << 16);
    ctrl.write_ctx32(input_ctx, ep_ctx_index, 1, ep_dw1);

    let int_dequeue = int_ring.dequeue();
    ctrl.write_ctx32(input_ctx, ep_ctx_index, 2, int_dequeue as u32);
    ctrl.write_ctx32(input_ctx, ep_ctx_index, 3, (int_dequeue >> 32) as u32);
    // Max ESIT Payload (high) / Average TRB Length (low) are both the max packet size (xHCI 1.2 §6.2.3.8, §4.14.2).
    let esit = info.ep.max_packet as u32;
    ctrl.write_ctx32(input_ctx, ep_ctx_index, 4, (esit << 16) | esit);
    int_ring
}

// Everything class-specific: where this sequence stops and the device's own driver starts.
fn bind(ctrl: &mut XhciController, state: Enumerating) {
    let (_, function) = state.parsed.expect("a configuration named a function");
    let rings = state.rings.expect("Configure Endpoint named this device's rings");
    // Whether a device came of it decides who keeps the slot; a refusal here would leak it.
    let bound = match (function, rings) {
        (Function::Msc(info), Rings::Msc(msc)) => {
            super::msc::bind(ctrl, state.ep0_ring, state.slot_id, state.block, msc, &info)
        }
        (Function::Hid(info), Rings::Hid(int_ring)) => bind_hid(ctrl, &state, &info, int_ring),
        // Rings are built from the function two acts earlier; nothing between can change it.
        _ => unreachable!("the rings were built for another function"),
    };
    if bound {
        finish(ctrl, state.port_idx, Some(state.slot_id));
    } else {
        refuse(ctrl, state.port_idx, state.slot_id);
    }
}

/// `true` if a device came of it — the caller gives the slot back if not.
fn bind_hid(
    ctrl: &mut XhciController,
    state: &Enumerating,
    info: &HidInterfaceInfo,
    int_ring: TrbRing,
) -> bool {
    let report = ctrl.dma().subview(state.block + DEV_REPORT, 8);
    let report_size = match info.protocol {
        HidType::Keyboard => 8,
        HidType::Mouse => 4,
        HidType::Tablet => 6,
    };
    let role = match info.protocol {
        HidType::Keyboard => HidRole::Keyboard,
        // A pointer with no free button-table entry can't be bound; sharing one would publish another's releases.
        HidType::Mouse | HidType::Tablet => match crate::mouse::PointerSource::claim() {
            Some(source) => HidRole::Pointer(source),
            None => {
                log!("xHCI: slot {} is past the pointers this machine can number, dropping it",
                    state.slot_id);
                return false;
            }
        },
    };
    let mut dev = HidDevice {
        slot_id: state.slot_id,
        port_idx: state.port_idx,
        block: state.block,
        int_ep_dci: info.ep.dci(),
        ep_addr: info.ep.addr,
        int_ring,
        ep0_ring: state.ep0_ring,
        report,
        report_size,
        role,
        prev_report: [0; 8],
        broke_with: None,
        failures: 0,
        completions: 0,
    };

    dev.requeue(&ctrl.db_base);
    // The ring offset is logged because two devices sharing one ring is invisible until their TRBs interleave.
    log!("xHCI: USB {} ready on slot {}, int_ring +{:#x}",
        hid_kind(info.protocol), state.slot_id, state.block + DEV_INT_RING);
    // Logged because a source derived from the slot id would merge two controllers' slot-1 devices into one.
    if let HidRole::Pointer(source) = dev.role {
        log!("xHCI: pointer on slot {} merges as source {}", state.slot_id, source.id());
    }
    ctrl.devices.push(dev);
    true
}

/// The enumeration is over; `slot` is recorded even on refusal, since only Disable Slot returns it.
pub(super) fn finish(ctrl: &mut XhciController, port_idx: u8, slot: Option<u8>) {
    ctrl.ports[port_idx as usize].enumerated(slot.and_then(NonZeroU8::new));
    // Clears flags other than PRC left set on a now-quiet port, so a later disconnect's CSC isn't already '1'.
    ctrl.acknowledge_port_read(port_idx);
}

/// Refuses the device, with the port left attached and no slot so it isn't re-enumerated next debounce.
pub(super) fn refuse(ctrl: &mut XhciController, port_idx: u8, slot_id: u8) {
    ctrl.ports[port_idx as usize].enumerated(None);
    ctrl.acknowledge_port_read(port_idx);
    // Released here, not at unplug: a refused device left plugged in would otherwise hold a slot for the rest of the boot.
    ctrl.submit_disable_slot(slot_id, super::AfterSlot::Refused);
}

/// Drops the outstanding enumeration for a port whose device has gone; the slot passes to the port for teardown.
pub(super) fn cancel_on(ctrl: &mut XhciController, port_idx: u8) {
    // An outstanding Enable Slot is left alone: its answer is the only carrier of the slot id, and cancelling here would leak it.
    let Some(What::Enumerating(state)) = ctrl.outstanding.what() else { return };
    if state.port_idx != port_idx {
        return;
    }
    let slot_id = state.slot_id;
    ctrl.outstanding.cancel();
    log!("xHCI: the enumeration on port {} is abandoned; its device has gone", port_idx + 1);
    // Only the slot, not `finish`'s acknowledge: that would clear the very change flag that brought us here.
    ctrl.ports[port_idx as usize].enumerated(NonZeroU8::new(slot_id));
}

/// Exercises configuration descriptors no attached device will ever hand us, since `parse_config` is pure.
#[cfg(feature = "boot-actuators")]
pub fn selftest() {
    /// (kind, config value, first DCI, second DCI); a tuple because `Function` has no equality.
    type Verdict = Option<(u8, u8, u8, u8)>;

    fn summarise(got: Option<(u8, Function)>) -> Verdict {
        match got? {
            (cfg, Function::Hid(h)) => Some((1, cfg, h.ep.dci(), 0)),
            (cfg, Function::Msc(m)) => Some((2, cfg, m.in_ep.dci(), m.out_ep.dci())),
        }
    }

    /// A config descriptor with `wTotalLength = total` and body: one interface then `eps` (address, transfer type).
    fn build(buf: &mut [u8; 64], class: (u8, u8, u8), eps: &[(u8, u8)], total: u16) -> usize {
        buf.fill(0);
        buf[..9].copy_from_slice(&[9, 2, total as u8, (total >> 8) as u8, 1, 0x42, 0, 0, 0]);
        buf[9..18].copy_from_slice(&[9, 4, 0, 0, eps.len() as u8, class.0, class.1, class.2, 0]);
        let mut at = 18;
        for &(addr, transfer) in eps {
            buf[at..at + 7].copy_from_slice(&[7, 5, addr, transfer, 64, 0, 8]);
            at += 7;
        }
        at
    }

    const MSC: (u8, u8, u8) = (0x08, 0x06, 0x50);
    const KBD: (u8, u8, u8) = (3, 1, 1);
    const CASES: usize = 9;

    let mut passed = 0usize;
    let mut buf = [0u8; 64];
    let mut check = |name: &str, desc: &[u8], want: Verdict| {
        let got = summarise(parse_config(desc));
        if got == want {
            passed += 1;
        } else {
            log!("xHCI: descriptor selftest FAILED on {name}: got {got:?}, want {want:?}");
        }
    };

    let len = build(&mut buf, MSC, &[(0x81, 2), (0x02, 2)], 32);
    check("an ordinary disk", &buf[..len], Some((2, 0x42, 3, 4)));

    let len = build(&mut buf, MSC, &[(0x80, 2), (0x02, 2)], 32);
    check("a bulk IN endpoint naming endpoint 0", &buf[..len], None);

    let len = build(&mut buf, MSC, &[(0x81, 2), (0x10, 2)], 32);
    check("a bulk OUT endpoint naming endpoint 0", &buf[..len], None);

    let len = build(&mut buf, MSC, &[(0x81, 3), (0x02, 3)], 32);
    check("a mass-storage interface with no bulk pair", &buf[..len], None);

    let len = build(&mut buf, KBD, &[(0x81, 3)], 25);
    check("an ordinary keyboard", &buf[..len], Some((1, 0x42, 3, 0)));

    let len = build(&mut buf, KBD, &[(0x80, 3)], 25);
    check("a keyboard whose interrupt endpoint is endpoint 0", &buf[..len], None);

    let len = build(&mut buf, KBD, &[(0x81, 3)], 25);
    buf[9] = 0;
    check("a descriptor claiming zero length", &buf[..len], None);

    let len = build(&mut buf, KBD, &[(0x81, 3)], u16::MAX);
    check("wTotalLength past the buffer", &buf[..len], Some((1, 0x42, 3, 0)));

    let len = build(&mut buf, KBD, &[(0x81, 3)], 25);
    check("a truncated final descriptor", &buf[..len - 3], None);

    log!("xHCI: descriptor selftest {passed}/{CASES} configurations parsed as required");
}
