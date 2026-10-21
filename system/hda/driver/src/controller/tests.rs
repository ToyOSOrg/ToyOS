//! The controller against a model of one: registers that answer as §3.3
//! defines them, a CORB the model fetches on a write pointer and a RIRB it
//! fills, and every misbehaviour a refusal names.

extern crate std;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::*;
use crate::fixture;
use crate::probe;

/// The laptop's GCAP: 9 output, 7 input, no bidirectional streams, 64OK.
const LAPTOP_GCAP: u16 = 0x9701;
const WINDOW: usize = 0x4000;
const GRANT_AT: u64 = 0x2000_0000;
/// Where the device reaches the ring soundserver lent.
const RING_AT: u64 = 0x4000_0000;

/// What the model's codecs answer one verb with: each a response and its
/// `Resp_Ex` word, written to the RIRB in order.
type Answer = Box<dyn FnMut(u32) -> Vec<(u32, u32)>>;

/// The solicited answer of a well-behaved codec: a word derived from the
/// verb, from the codec it was sent to.
fn echo(verb: u32) -> Vec<(u32, u32)> {
    vec![(verb ^ 0xA5A5_A5A5, verb >> 28)]
}

struct State {
    regs: Vec<u8>,
    mem: Vec<u8>,
    device_base: u64,
    codecs: u16,
    answer: Answer,
    /// Every verb the model fetched from the CORB, in order.
    fetched: Vec<u32>,
    /// Every register access, `true` for a write, with its offset.
    accesses: Vec<(bool, usize)>,
    crst_stuck: Option<bool>,
    rprst_never_sets: bool,
    srst_stuck: bool,
    run_stuck: bool,
    overrun: bool,
    rirbwp_override: Option<u16>,
    srst_seen: bool,
}

#[derive(Clone)]
struct Model(Rc<RefCell<State>>);

impl Model {
    fn new(gcap: u16, codecs: u16) -> Self {
        let mut regs = vec![0u8; WINDOW];
        regs[GCAP..GCAP + 2].copy_from_slice(&gcap.to_le_bytes());
        // Every ring size offered (§3.3.24), and the controller left running
        // by whoever had it before.
        regs[CORBSIZE] = 0x70;
        regs[RIRBSIZE] = 0x70;
        regs[GCTL] = 1;
        Self(Rc::new(RefCell::new(State {
            regs,
            mem: vec![0u8; GRANT_BYTES],
            device_base: GRANT_AT,
            codecs,
            answer: Box::new(echo),
            fetched: Vec::new(),
            accesses: Vec::new(),
            crst_stuck: None,
            rprst_never_sets: false,
            srst_stuck: false,
            run_stuck: false,
            overrun: false,
            rirbwp_override: None,
            srst_seen: false,
        })))
    }

    fn laptop() -> Self {
        Self::new(LAPTOP_GCAP, 0b101)
    }

    fn with(self, f: impl FnOnce(&mut State)) -> Self {
        f(&mut self.0.borrow_mut());
        self
    }

    fn regs(&self) -> ModelRegs {
        ModelRegs(self.clone())
    }

    fn mem(&self) -> ModelMem {
        ModelMem(self.clone())
    }

    fn reg32(&self, at: usize) -> u32 {
        let s = self.0.borrow();
        u32::from_le_bytes(s.regs[at..at + 4].try_into().unwrap())
    }

    fn reg16(&self, at: usize) -> u16 {
        let s = self.0.borrow();
        u16::from_le_bytes(s.regs[at..at + 2].try_into().unwrap())
    }

    fn reg8(&self, at: usize) -> u8 {
        self.0.borrow().regs[at]
    }

    fn set32(&self, at: usize, value: u32) {
        self.0.borrow_mut().regs[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn set8(&self, at: usize, value: u8) {
        self.0.borrow_mut().regs[at] = value;
    }

    fn mem32(&self, at: usize) -> u32 {
        u32::from_le_bytes(self.0.borrow().mem[at..at + 4].try_into().unwrap())
    }

    fn mem64(&self, at: usize) -> u64 {
        u64::from_le_bytes(self.0.borrow().mem[at..at + 8].try_into().unwrap())
    }

    fn fetched(&self) -> Vec<u32> {
        self.0.borrow().fetched.clone()
    }
}

impl State {
    fn get(&self, at: usize, width: usize) -> u32 {
        let mut word = [0u8; 4];
        word[..width].copy_from_slice(&self.regs[at..at + width]);
        u32::from_le_bytes(word)
    }

    fn put(&mut self, at: usize, width: usize, value: u32) {
        self.regs[at..at + width].copy_from_slice(&value.to_le_bytes()[..width]);
    }

    fn write(&mut self, at: usize, width: usize, value: u32) {
        let old = self.get(at, width);
        let stream = (SD_BASE..WINDOW).contains(&at).then(|| (at - SD_BASE) % SD_STRIDE);
        match (at, stream) {
            (GCAP, _) => {}
            (GCTL, _) => {
                let crst = self.crst_stuck.unwrap_or(value & GCTL_CRST != 0);
                if crst && old & GCTL_CRST == 0 {
                    let statests = self.get(STATESTS, 2) as u16 | self.codecs;
                    self.put(STATESTS, 2, u32::from(statests));
                }
                self.put(GCTL, 4, value & !GCTL_CRST | u32::from(crst));
            }
            (STATESTS, _) | (RIRBSTS, _) => self.put(at, width, old & !value),
            (CORBSIZE, _) | (RIRBSIZE, _) => self.put(at, 1, old & 0xF0 | value & 0x0F),
            (CORBRP, _) => {
                let reset = value as u16 & CORBRP_RST != 0 && !self.rprst_never_sets;
                self.put(CORBRP, 2, if reset { u32::from(CORBRP_RST) } else { 0 });
            }
            (RIRBWP, _) => {
                if value as u16 & RIRBWP_RST != 0 {
                    self.put(RIRBWP, 2, 0);
                }
            }
            (CORBWP, _) => {
                self.put(CORBWP, 2, value);
                if self.regs[CORBCTL] & CORBCTL_RUN != 0 {
                    self.fetch();
                }
            }
            (_, Some(SD_STS)) => self.put(at, 1, old & !value),
            (_, Some(SD_CTL)) => {
                let mut byte = value as u8;
                if byte & SD_CTL_SRST != 0 {
                    self.srst_seen = true;
                }
                if self.srst_stuck {
                    byte = byte & !SD_CTL_SRST | old as u8 & SD_CTL_SRST;
                }
                if self.run_stuck && old as u8 & SD_CTL_RUN != 0 {
                    byte |= SD_CTL_RUN;
                }
                self.put(at, 1, u32::from(byte));
            }
            _ => self.put(at, width, value),
        }
    }

    fn entries(size: u8) -> u16 {
        [2, 16, 256][(size & 0b11) as usize]
    }

    /// §4.4.1: fetch until the read pointer meets the write pointer, and
    /// write what each verb is answered with to the RIRB.
    fn fetch(&mut self) {
        let corb = Self::entries(self.regs[CORBSIZE]);
        let rirb = Self::entries(self.regs[RIRBSIZE]);
        let wp = self.get(CORBWP, 2) as u16 & POINTER;
        let mut rp = self.get(CORBRP, 2) as u16 & POINTER;
        while rp != wp {
            rp = (rp + 1) % corb;
            let at = CORB + rp as usize * 4;
            let verb = u32::from_le_bytes(self.mem[at..at + 4].try_into().unwrap());
            self.fetched.push(verb);
            for (response, extended) in (self.answer)(verb) {
                let written = (self.get(RIRBWP, 2) as u16 + 1) % rirb;
                let at = RIRB + written as usize * 8;
                self.mem[at..at + 4].copy_from_slice(&response.to_le_bytes());
                self.mem[at + 4..at + 8].copy_from_slice(&extended.to_le_bytes());
                self.put(RIRBWP, 2, u32::from(written));
            }
        }
        self.put(CORBRP, 2, u32::from(rp));
        if let Some(written) = self.rirbwp_override {
            self.put(RIRBWP, 2, u32::from(written));
        }
        if self.overrun {
            let sts = self.regs[RIRBSTS] | RIRBSTS_OIS;
            self.regs[RIRBSTS] = sts;
        }
    }
}

struct ModelRegs(Model);

impl ModelRegs {
    fn read(&self, at: usize, width: usize) -> u32 {
        assert!(at + width <= WINDOW && at.is_multiple_of(width), "an access at {at:#x} of {width}");
        let mut s = self.0 .0.borrow_mut();
        s.accesses.push((false, at));
        s.get(at, width)
    }

    fn write(&self, at: usize, width: usize, value: u32) {
        assert!(at + width <= WINDOW && at.is_multiple_of(width), "an access at {at:#x} of {width}");
        let mut s = self.0 .0.borrow_mut();
        s.accesses.push((true, at));
        s.write(at, width, value);
    }
}

impl Registers for ModelRegs {
    fn bytes(&self) -> usize {
        WINDOW
    }
    fn read8(&self, at: usize) -> u8 {
        self.read(at, 1) as u8
    }
    fn read16(&self, at: usize) -> u16 {
        self.read(at, 2) as u16
    }
    fn read32(&self, at: usize) -> u32 {
        self.read(at, 4)
    }
    fn write8(&self, at: usize, value: u8) {
        self.write(at, 1, value.into());
    }
    fn write16(&self, at: usize, value: u16) {
        self.write(at, 2, value.into());
    }
    fn write32(&self, at: usize, value: u32) {
        self.write(at, 4, value);
    }
}

struct ModelMem(Model);

impl ModelMem {
    fn bytes_at<const N: usize>(&self, at: usize) -> [u8; N] {
        self.0 .0.borrow().mem[at..at + N].try_into().unwrap()
    }

    fn put(&self, at: usize, bytes: &[u8]) {
        self.0 .0.borrow_mut().mem[at..at + bytes.len()].copy_from_slice(bytes);
    }
}

impl DmaBuffers for ModelMem {
    fn bytes(&self) -> usize {
        GRANT_BYTES
    }
    fn device_addr(&self, at: usize) -> u64 {
        self.0 .0.borrow().device_base + at as u64
    }
    fn read16(&self, at: usize) -> u16 {
        u16::from_le_bytes(self.bytes_at(at))
    }
    fn read32(&self, at: usize) -> u32 {
        u32::from_le_bytes(self.bytes_at(at))
    }
    fn read64(&self, at: usize) -> u64 {
        u64::from_le_bytes(self.bytes_at(at))
    }
    fn write16(&self, at: usize, value: u16) {
        self.put(at, &value.to_le_bytes());
    }
    fn write32(&self, at: usize, value: u32) {
        self.put(at, &value.to_le_bytes());
    }
    fn write64(&self, at: usize, value: u64) {
        self.put(at, &value.to_le_bytes());
    }
    fn publish(&self) {}
    fn observe(&self) {}
}

fn brought_up(model: &Model) -> Reset {
    reset(&model.regs(), || {}).expect("the model comes out of reset")
}

fn linked(model: &Model) -> Link<ModelRegs, ModelMem> {
    let reset = brought_up(model);
    Link::open(model.regs(), model.mem(), &reset).expect("the model's rings come up")
}

fn verb(codec: u8, node: u8) -> Verb {
    Verb::short(crate::verb::Address::new(codec).unwrap(), crate::verb::Node(node), 0xF00, 0x04)
}

// ---- reset (§4.2.2, §4.3) ----

#[test]
fn a_reset_reads_every_codec_that_asked_for_an_address_after_the_detect_wait() {
    let model = Model::laptop();
    let probe = model.clone();
    let mut waited = false;
    let reset = reset(&model.regs(), || {
        // §4.3's 25 frames count from CRST reading 1.
        assert_eq!(probe.reg32(GCTL) & GCTL_CRST, GCTL_CRST);
        waited = true;
    })
    .unwrap();
    assert!(waited);
    assert_eq!(reset, Reset { gcap: Gcap(LAPTOP_GCAP), statests: 0b101 });
    assert_eq!(model.reg32(GCTL) & GCTL_UNSOL, 0, "unsolicited responses are dropped");
    assert_eq!(model.reg32(INTCTL), 0);
    assert_eq!(model.reg16(STATESTS), 0, "the flags read are cleared");
}

#[test]
fn every_dma_engine_is_stopped_before_the_reset() {
    let model = Model::laptop();
    model.set8(CORBCTL, CORBCTL_RUN);
    model.set8(SD_BASE + 3 * SD_STRIDE, SD_CTL_RUN);
    brought_up(&model);
    let s = model.0.borrow();
    let entered = s.accesses.iter().position(|&(write, at)| write && at == GCTL).unwrap();
    let stopped = s.accesses.iter().rposition(|&(write, at)| write && at == SD_BASE + 3 * SD_STRIDE).unwrap();
    assert!(stopped < entered);
    assert_eq!(s.regs[SD_BASE + 3 * SD_STRIDE] & SD_CTL_RUN, 0);
}

#[test]
fn a_controller_that_never_enters_or_leaves_reset_is_refused() {
    let stuck = Model::laptop().with(|s| s.crst_stuck = Some(true));
    assert!(matches!(reset(&stuck.regs(), || {}), Err(Refusal::NeverEnteredReset { .. })));
    let stuck = Model::laptop().with(|s| s.crst_stuck = Some(false));
    assert!(matches!(reset(&stuck.regs(), || {}), Err(Refusal::NeverLeftReset { .. })));
}

#[test]
fn a_window_reading_all_ones_or_a_link_with_no_codec_is_refused() {
    let gone = Model::new(u16::MAX, 0b1);
    assert_eq!(reset(&gone.regs(), || {}), Err(Refusal::AllOnes));
    let empty = Model::new(LAPTOP_GCAP, 0);
    assert_eq!(reset(&empty.regs(), || {}), Err(Refusal::NoCodec));
}

// ---- the link (§4.4) ----

#[test]
fn each_verb_is_written_after_the_write_pointer_and_answered_with_its_own_response() {
    let model = Model::laptop();
    let mut link = linked(&model);
    let verbs = [verb(0, 0x00), verb(2, 0x01), verb(0, 0x14)];
    for v in verbs {
        assert_eq!(link.send(v), Ok(v.raw() ^ 0xA5A5_A5A5));
    }
    // §4.4.1.4: the first command goes at offset (0 + 1) * 4.
    assert_eq!(model.mem32(CORB), 0);
    for (i, v) in verbs.iter().enumerate() {
        assert_eq!(model.mem32(CORB + (i + 1) * 4), v.raw());
    }
    assert_eq!(model.fetched(), verbs.map(Verb::raw));
}

#[test]
fn the_rings_are_where_the_grant_puts_them_and_sized_as_large_as_offered() {
    let model = Model::laptop();
    linked(&model);
    assert_eq!(model.reg32(CORBLBASE), GRANT_AT as u32 + CORB as u32);
    assert_eq!(model.reg32(RIRBLBASE), GRANT_AT as u32 + RIRB as u32);
    assert_eq!(model.reg8(CORBSIZE) & 0b11, 0b10);
    assert_eq!(model.reg8(RIRBSIZE) & 0b11, 0b10);
    assert_eq!(model.reg8(CORBCTL), CORBCTL_RUN);
    assert_eq!(model.reg8(RIRBCTL), RIRBCTL_DMAEN | RIRBCTL_RINTCTL);
    assert_eq!(model.reg16(RINTCNT), 1);
    assert_eq!(model.reg32(INTCTL) & 1 << 30, 0, "CIE: the response flag interrupts nothing");
}

#[test]
fn both_rings_wrap_at_every_size_a_controller_may_offer() {
    for cap in [0x10, 0x20, 0x40] {
        let model = Model::laptop().with(|s| {
            s.regs[CORBSIZE] = cap;
            s.regs[RIRBSIZE] = cap;
        });
        let mut link = linked(&model);
        for n in 0..600u32 {
            let v = verb((n % 3) as u8, n as u8);
            assert_eq!(link.send(v), Ok(v.raw() ^ 0xA5A5_A5A5), "verb {n} on rings of cap {cap:#x}");
        }
    }
}

#[test]
fn a_size_register_offering_no_size_is_refused() {
    let model = Model::laptop().with(|s| s.regs[RIRBSIZE] = 0x80);
    let reset = brought_up(&model);
    assert_eq!(
        Link::open(model.regs(), model.mem(), &reset).err(),
        Some(Refusal::RingSize { ring: Ring::Rirb, size: 0x80 })
    );
}

#[test]
fn a_read_pointer_reset_that_never_reads_back_is_refused() {
    let model = Model::laptop().with(|s| s.rprst_never_sets = true);
    let reset = brought_up(&model);
    assert_eq!(
        Link::open(model.regs(), model.mem(), &reset).err(),
        Some(Refusal::ReadPointerReset { set: true })
    );
}

#[test]
fn a_grant_above_four_gib_is_refused_only_where_the_controller_is_narrow() {
    let high = 0x1_2000_0000;
    let narrow = Model::new(LAPTOP_GCAP & !1, 0b1).with(|s| s.device_base = high);
    let reset = brought_up(&narrow);
    assert_eq!(
        Link::open(narrow.regs(), narrow.mem(), &reset).err(),
        Some(Refusal::Beyond32Bits { device_addr: high + CORB as u64 })
    );
    let wide = Model::laptop().with(|s| s.device_base = high);
    linked(&wide);
    assert_eq!(wide.reg32(CORBUBASE), 1);
    assert_eq!(wide.reg32(RIRBLBASE), 0x2000_0000 + RIRB as u32);
}

/// Every malformed answer is refused by name, and the link answers nothing
/// after it: the CORB is not touched again.
fn refused_for_good(model: Model, want: LinkFault) {
    let mut link = linked(&model);
    let v = verb(2, 0x14);
    assert_eq!(link.send(v), Err(want));
    let sent = model.fetched().len();
    assert_eq!(link.send(verb(0, 0)), Err(want));
    assert_eq!(model.fetched().len(), sent, "a refused link sent another verb");
    assert_eq!(link.refused(), Some(want));
}

#[test]
fn an_answer_from_another_codec_is_refused() {
    let model = Model::laptop().with(|s| s.answer = Box::new(|v| vec![(0, (v >> 28) ^ 1)]));
    refused_for_good(model, LinkFault::WrongCodec { asked: 2, answered: 3 });
}

#[test]
fn an_unsolicited_response_is_refused() {
    let model = Model::laptop().with(|s| s.answer = Box::new(|v| vec![(0, v >> 28 | RESP_UNSOL)]));
    refused_for_good(model, LinkFault::Unsolicited);
}

#[test]
fn a_second_response_to_one_verb_is_refused() {
    let model = Model::laptop().with(|s| s.answer = Box::new(|v| [echo(v), echo(v)].concat()));
    refused_for_good(model, LinkFault::Extra { responses: 2 });
}

/// A codec that answers nothing is silent and no more: nothing arrived, so
/// the next verb's answer is its own. One that answers after all is a second
/// response to the next verb, and that ends the link.
#[test]
fn a_silent_codec_leaves_the_link_to_the_others_until_it_answers_late() {
    let late: Rc<RefCell<Vec<(u32, u32)>>> = Rc::default();
    let held = late.clone();
    let model = Model::laptop().with(move |s| {
        s.answer = Box::new(move |v| match v >> 28 {
            3 => {
                held.borrow_mut().extend(echo(v));
                Vec::new()
            }
            _ => [core::mem::take(&mut *held.borrow_mut()), echo(v)].concat(),
        })
    });
    let mut link = linked(&model);
    let silent = verb(3, 0x01);
    assert_eq!(link.send(silent), Err(LinkFault::Silent { verb: silent.raw() }));
    assert_eq!(link.refused(), None);
    late.borrow_mut().clear();
    let v = verb(0, 0x02);
    assert_eq!(link.send(v), Ok(v.raw() ^ 0xA5A5_A5A5));

    assert!(link.send(silent).is_err());
    assert_eq!(link.send(v), Err(LinkFault::Extra { responses: 2 }));
    assert_eq!(link.refused(), Some(LinkFault::Extra { responses: 2 }));
}

#[test]
fn an_overrun_is_refused() {
    let model = Model::laptop().with(|s| s.overrun = true);
    refused_for_good(model, LinkFault::Overrun);
}

#[test]
fn a_write_pointer_outside_a_small_ring_is_refused() {
    let model = Model::laptop().with(|s| {
        s.regs[RIRBSIZE] = 0x20;
        s.rirbwp_override = Some(200);
    });
    refused_for_good(model, LinkFault::PointerOutsideRing { written: 200, entries: 16 });
}

/// The AMD bench's analog controller, answering what it answered: walked over
/// the rings, its codec is the graph soundserver built on that boot.
#[test]
fn a_walk_over_the_rings_builds_the_graph_the_bench_built() {
    let mut recorded = fixture::Recorded { text: fixture::AMD_BENCH, controller: 1 };
    let model = Model::laptop().with(|s| {
        s.codecs = 0b1;
        s.answer = Box::new(move |raw| {
            let codec = crate::verb::Address::new((raw >> 28) as u8).unwrap();
            let node = crate::verb::Node((raw >> 20) as u8);
            let answer = probe::Verbs::get(&mut recorded, codec, node, (raw >> 8 & 0xFFF) as u16, raw as u8)
                .expect("every recorded verb was answered");
            vec![(answer.raw(), raw >> 28)]
        });
    });
    let reset = brought_up(&model);
    let mut link = Link::open(model.regs(), model.mem(), &reset).unwrap();
    let walked: Vec<_> = probe::enumerate(&mut link, reset.statests).into_iter().map(Result::unwrap).collect();
    assert_eq!(link.refused(), None);
    assert_eq!(alloc::format!("{walked:?}"), alloc::format!("{:?}", fixture::amd_bench()[1]));
}

// ---- the stream (§3.3.35–§3.3.43, §4.5) ----

/// The laptop's first output descriptor: after its seven input ones.
const SD: usize = SD_BASE + 7 * SD_STRIDE;

fn streaming(model: &Model) -> Stream<ModelRegs> {
    let reset = brought_up(model);
    Stream::open(model.regs(), &model.mem(), RING_AT, &reset, 0x4011).expect("the model's stream opens")
}

#[test]
fn the_stream_is_reset_and_pointed_at_the_grants_periods_before_it_runs() {
    let model = Model::laptop();
    model.set8(SD + SD_STS, SD_STS_CLEAR);
    streaming(&model);
    assert!(model.0.borrow().srst_seen);
    for i in 0..PERIODS {
        let slot = BDL + i * 16;
        assert_eq!(model.mem64(slot), RING_AT + (i * PERIOD_BYTES) as u64);
        assert_eq!(model.mem32(slot + 8), PERIOD_BYTES as u32);
        assert_eq!(model.mem32(slot + 12), 1, "IOC on every period");
    }
    assert_eq!(model.reg32(SD + SD_BDPL), GRANT_AT as u32 + BDL as u32);
    assert_eq!(model.reg32(SD + SD_BDPU), 0);
    assert_eq!(model.reg32(SD + SD_CBL), (PERIODS * PERIOD_BYTES) as u32);
    assert_eq!(model.reg16(SD + SD_LVI), PERIODS as u16 - 1);
    assert_eq!(model.reg16(SD + SD_FMT), 0x4011);
    assert_eq!(model.reg8(SD + SD_CTL_STREAM), STREAM_TAG << 4);
    assert_eq!(model.reg8(SD + SD_CTL), SD_CTL_IOCE | SD_CTL_FEIE | SD_CTL_DEIE, "not running");
    assert_eq!(model.reg8(SD + SD_STS), 0);
    // §3.3.14: SIE bit 7 is the first output stream behind seven inputs.
    assert_eq!(model.reg32(INTCTL), INTCTL_GIE | 1 << 7);
}

#[test]
fn a_controller_without_an_output_stream_or_whose_stream_stays_in_reset_is_refused() {
    let model = Model::new(0x0701, 0b1);
    let reset = brought_up(&model);
    assert!(matches!(
        Stream::open(model.regs(), &model.mem(), RING_AT, &reset, 0x4011),
        Err(Refusal::NoOutputStream { .. })
    ));
    let model = Model::laptop().with(|s| s.srst_stuck = true);
    let reset = brought_up(&model);
    assert!(matches!(
        Stream::open(model.regs(), &model.mem(), RING_AT, &reset, 0x4011),
        Err(Refusal::StreamReset { entered: true })
    ));
}

#[test]
fn a_ring_the_engine_cannot_be_pointed_at_is_refused() {
    let model = Model::laptop();
    let reset = brought_up(&model);
    assert!(matches!(
        Stream::open(model.regs(), &model.mem(), RING_AT + 64, &reset, 0x4011),
        Err(Refusal::RingUnaligned { device_addr }) if device_addr == RING_AT + 64
    ));
    let narrow = Model::new(LAPTOP_GCAP & !1, 0b1);
    let reset = brought_up(&narrow);
    assert!(matches!(
        Stream::open(narrow.regs(), &narrow.mem(), 0xFFFF_FF80, &reset, 0x4011),
        Err(Refusal::Beyond32Bits { .. })
    ));
}

#[test]
fn played_names_the_periods_between_two_positions_and_clears_the_status() {
    let model = Model::laptop();
    let mut stream = streaming(&model);
    stream.start().unwrap();
    assert_eq!(model.reg8(SD + SD_CTL) & SD_CTL_RUN, SD_CTL_RUN);
    model.set32(SD + SD_LPIB, 3 * PERIOD_BYTES as u32 + 10);
    model.set8(SD + SD_STS, SD_STS_BCIS);
    assert_eq!(stream.played(), Ok(Played { mask: 0b111, underran: false }));
    assert_eq!(model.reg8(SD + SD_STS), 0);
    // §3.3.37: CBL itself is the wrap.
    model.set32(SD + SD_LPIB, (PERIODS * PERIOD_BYTES) as u32);
    model.set8(SD + SD_STS, SD_STS_BCIS | SD_STS_FIFOE);
    assert_eq!(stream.played(), Ok(Played { mask: 0b1111_1000, underran: true }));
}

/// A period completing between the position read and the status clear is
/// counted at the next look, whose interrupt is after it: never counted now
/// under an interrupt older than itself.
#[test]
fn the_position_is_read_before_the_status_is_cleared() {
    let model = Model::laptop();
    let mut stream = streaming(&model);
    stream.start().unwrap();
    model.set8(SD + SD_STS, SD_STS_BCIS);
    model.0.borrow_mut().accesses.clear();
    stream.played().unwrap();
    let s = model.0.borrow();
    let read = s.accesses.iter().position(|&a| a == (false, SD + SD_LPIB)).unwrap();
    let cleared = s.accesses.iter().position(|&a| a == (true, SD + SD_STS)).unwrap();
    assert!(read < cleared);
}

#[test]
fn a_start_resumes_from_the_period_the_engine_stopped_in() {
    let model = Model::laptop();
    let mut stream = streaming(&model);
    model.set32(SD + SD_LPIB, 5 * PERIOD_BYTES as u32 + 3);
    model.set8(SD + SD_STS, SD_STS_BCIS);
    stream.start().unwrap();
    assert_eq!(model.reg8(SD + SD_STS), 0, "a status left from before the stop is cleared");
    model.set32(SD + SD_LPIB, 6 * PERIOD_BYTES as u32);
    assert_eq!(stream.played().unwrap().mask, 1 << 5);
    stream.stop().unwrap();
    assert_eq!(model.reg8(SD + SD_CTL) & SD_CTL_RUN, 0);
    assert!(!stream.running());
}

#[test]
fn a_descriptor_error_or_a_position_outside_the_buffer_ends_the_stream() {
    let model = Model::laptop();
    let mut stream = streaming(&model);
    stream.start().unwrap();
    model.set8(SD + SD_STS, SD_STS_DESE);
    assert_eq!(stream.played(), Err(StreamFault::Descriptor));
    model.set32(SD + SD_LPIB, (PERIODS * PERIOD_BYTES) as u32 + 2);
    assert_eq!(
        stream.played(),
        Err(StreamFault::PositionOutsideBuffer { position: (PERIODS * PERIOD_BYTES) as u32 + 2 })
    );
}

#[test]
fn a_stream_that_never_stops_is_refused() {
    let model = Model::laptop().with(|s| s.run_stuck = true);
    let mut stream = streaming(&model);
    stream.start().unwrap();
    assert_eq!(stream.stop(), Err(StreamFault::NeverStopped));
}

/// 64OK decides the width, except for a function a measurement contradicts,
/// which is driven at none.
#[test]
fn the_address_width_is_64ok_unless_it_is_contradicted() {
    assert_eq!(address_bits(0x8086, 0xa0c8, Gcap(LAPTOP_GCAP)), Ok(64));
    assert_eq!(address_bits(0x8086, 0xa0c8, Gcap(LAPTOP_GCAP & !1)), Ok(32));
    assert_eq!(address_bits(0x1022, 0x15e3, Gcap(0x4401)), Ok(64));
    let refused = address_bits(0x1002, 0x1637, Gcap(0x4001)).unwrap_err();
    assert_eq!((refused.vendor, refused.device), (0x1002, 0x1637));
}
