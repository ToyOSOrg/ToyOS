//! The driver against a model of the device that answers on the doorbell, or
//! on the holder's wait, with §5.7's structures as the oracle for every word
//! it is sent and every answer a device can lie in.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use toyos_device_memory::DmaBuffers;
use toyos_virtio::queue::{Published, UsedRefusal};

use crate::wire::{self, Command, Rect};
use crate::{Claim, Declined, Gpu, Output, Queues, Refusal, CONTROL_PARTS, CONTROL_QUEUE_SIZE, CURSOR_PARTS, CURSOR_QUEUE_SIZE};

/// Where the device reaches the grant's first byte.
const BASE: u64 = 0x4000_0000;
/// Where the device reaches the first region the holder lends.
const LENT: u64 = 0x1_0000_0000;

/// The grant, as both the driver and the model reach it.
#[derive(Clone)]
struct Mem(Rc<RefCell<Vec<u8>>>);

impl Mem {
    fn new() -> Self {
        // Anything but zero, so a part the driver forgets to clear shows.
        Self(Rc::new(RefCell::new(vec![0xA5; crate::GRANT_BYTES])))
    }

    fn get(&self, at: usize, bytes: usize) -> u64 {
        let mem = self.0.borrow();
        let mut word = [0u8; 8];
        word[..bytes].copy_from_slice(&mem[at..at + bytes]);
        u64::from_le_bytes(word)
    }

    fn set(&self, at: usize, bytes: usize, value: u64) {
        self.0.borrow_mut()[at..at + bytes].copy_from_slice(&value.to_le_bytes()[..bytes]);
    }

    fn words_at(&self, addr: u64, len: u32) -> Vec<u32> {
        let at = (addr - BASE) as usize;
        (0..len as usize / 4).map(|n| self.get(at + 4 * n, 4) as u32).collect()
    }
}

impl DmaBuffers for Mem {
    fn bytes(&self) -> usize {
        crate::GRANT_BYTES
    }
    fn device_addr(&self, at: usize) -> u64 {
        BASE + at as u64
    }
    fn read16(&self, at: usize) -> u16 {
        self.get(at, 2) as u16
    }
    fn read32(&self, at: usize) -> u32 {
        self.get(at, 4) as u32
    }
    fn read64(&self, at: usize) -> u64 {
        self.get(at, 8)
    }
    fn write16(&self, at: usize, value: u16) {
        self.set(at, 2, value as u64)
    }
    fn write32(&self, at: usize, value: u32) {
        self.set(at, 4, value as u64)
    }
    fn write64(&self, at: usize, value: u64) {
        self.set(at, 8, value)
    }
    fn publish(&self) {}
    fn observe(&self) {}
}

/// One command, as the device read it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Sent {
    queue: u16,
    code: u32,
    flags: u32,
    fence: u64,
    payload: Vec<u32>,
}

impl Sent {
    /// The resource the command names, where §5.7.6.8 puts it.
    fn resource(&self) -> u32 {
        let at = match self.code {
            SET_SCANOUT => 5,
            FLUSH => 4,
            TRANSFER => 6,
            DISPLAY_INFO | GET_EDID | UPDATE_CURSOR | MOVE_CURSOR => return 0,
            _ => 0,
        };
        self.payload[at]
    }
}

/// What the holder and the device saw, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ev {
    /// A command, by its code and the resource it names (0 for none).
    Command(u32, u32),
    Lend(u64),
    Unlend(u64),
    EventsRead,
    EventsClear(u32),
    Wait,
}

/// How the model answers one command.
struct Reply {
    answer: u32,
    flags: Option<u32>,
    fence: Option<u64>,
    /// The `len` it reports, where it is not the answer's own.
    len: Option<u32>,
}

fn ok() -> Reply {
    Reply { answer: 0, flags: None, fence: None, len: None }
}

/// QEMU's answer to every command: success, the fence echoed, and on the
/// cursor queue nothing at all.
fn answered(sent: &Sent) -> Reply {
    let answer = match sent.code {
        0x0100 => wire::RESP_OK_DISPLAY_INFO,
        0x010a => wire::RESP_OK_EDID,
        _ => wire::RESP_OK_NODATA,
    };
    Reply { answer, ..ok() }
}

type Answering = Box<dyn FnMut(&Sent, &mut Device) -> Reply>;

/// The device's registers and what it shows.
struct Device {
    events: u32,
    /// Scanout 0's preferred size and whether it is enabled.
    preferred: (u32, u32, bool),
    edid: [u8; 128],
    /// Raised at the next wait, as an interrupt that lands mid-command.
    raise_at_wait: u32,
}

struct State {
    taken: [u16; 2],
    used: [u16; 2],
    sent: Vec<Sent>,
    trace: Vec<Ev>,
    /// Chains held until the next wait, where the device is slow.
    deferred: bool,
    held: Vec<(u16, u16)>,
    /// What the holder's wait answers.
    silent: bool,
    /// `None` is QEMU: no cursor answer. `Some` writes this `len` and type.
    cursor_answer: Option<(u32, u32)>,
    device: Device,
    lent: u64,
    refuse_lend: bool,
    answer: Answering,
}

#[derive(Clone)]
struct Model {
    mem: Mem,
    state: Rc<RefCell<State>>,
}

impl Model {
    fn new() -> Self {
        Self::answering(|sent, _| answered(sent))
    }

    fn answering(answer: impl FnMut(&Sent, &mut Device) -> Reply + 'static) -> Self {
        let mut edid = [0u8; 128];
        edid[..8].copy_from_slice(&[0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0]);
        edid[127] = 0x42;
        Self {
            mem: Mem::new(),
            state: Rc::new(RefCell::new(State {
                taken: [0; 2],
                used: [0; 2],
                sent: Vec::new(),
                trace: Vec::new(),
                deferred: false,
                held: Vec::new(),
                silent: false,
                cursor_answer: None,
                device: Device { events: 0, preferred: (1280, 800, true), edid, raise_at_wait: 0 },
                lent: 0,
                refuse_lend: false,
                answer: Box::new(answer),
            })),
        }
    }

    fn parts(queue: u16) -> (crate::Parts, u16) {
        match queue {
            wire::CONTROL_QUEUE => (CONTROL_PARTS, CONTROL_QUEUE_SIZE),
            wire::CURSOR_QUEUE => (CURSOR_PARTS, CURSOR_QUEUE_SIZE),
            other => panic!("model: queue {other} was never configured"),
        }
    }

    /// The chain at `head`: §2.7.5's descriptors, `(addr, len, writable)`.
    fn chain(&self, queue: u16, head: u16) -> Vec<(u64, u32, bool)> {
        let (parts, size) = Self::parts(queue);
        let mut descs = Vec::new();
        let mut at = head;
        loop {
            assert!(at < size && descs.len() < size as usize, "model: a chain off the table");
            let d = parts.desc + at as usize * 16;
            let flags = self.mem.get(d + 12, 2);
            descs.push((self.mem.get(d, 8), self.mem.get(d + 8, 4) as u32, flags & 2 != 0));
            if flags & 1 == 0 {
                return descs;
            }
            at = self.mem.get(d + 14, 2) as u16;
        }
    }

    /// Take every entry made available on `queue` since the last look.
    fn take(&self, queue: u16) {
        let (parts, size) = Self::parts(queue);
        loop {
            let avail = self.mem.get(parts.avail + 2, 2) as u16;
            let taken = self.state.borrow().taken[queue as usize];
            if taken == avail {
                return;
            }
            let head = self.mem.get(parts.avail + 4 + 2 * (taken % size) as usize, 2) as u16;
            self.state.borrow_mut().taken[queue as usize] = taken.wrapping_add(1);
            let chain = self.chain(queue, head);
            assert_eq!(chain.len(), 2, "model: every chain is a request and an answer");
            assert!(!chain[0].2 && chain[1].2, "model: §2.7.4.2: the request is read, the answer written");
            let words = self.mem.words_at(chain[0].0, chain[0].1);
            let sent = Sent {
                queue,
                code: words[0],
                flags: words[1],
                fence: u64::from(words[2]) | u64::from(words[3]) << 32,
                payload: words[6..].to_vec(),
            };
            let mut state = self.state.borrow_mut();
            state.trace.push(Ev::Command(sent.code, sent.resource()));
            state.sent.push(sent);
            if state.deferred {
                state.held.push((queue, head));
            } else {
                drop(state);
                self.answer(queue, head);
            }
        }
    }

    /// Answer the chain at `head` and put it on the used ring.
    fn answer(&self, queue: u16, head: u16) {
        let chain = self.chain(queue, head);
        let words = self.mem.words_at(chain[0].0, chain[0].1);
        let sent = Sent {
            queue,
            code: words[0],
            flags: words[1],
            fence: u64::from(words[2]) | u64::from(words[3]) << 32,
            payload: words[6..].to_vec(),
        };
        let (answer_addr, room) = (chain[1].0, chain[1].1);
        let len = if queue == wire::CURSOR_QUEUE {
            match self.state.borrow().cursor_answer {
                None => 0,
                Some((len, answer)) => {
                    self.mem.set((answer_addr - BASE) as usize, 4, answer as u64);
                    len
                }
            }
        } else {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            let reply = (state.answer)(&sent, &mut state.device);
            let mut out = vec![
                reply.answer,
                reply.flags.unwrap_or(sent.flags),
                reply.fence.unwrap_or(sent.fence) as u32,
                (reply.fence.unwrap_or(sent.fence) >> 32) as u32,
                0,
                0,
            ];
            if reply.answer == wire::RESP_OK_DISPLAY_INFO {
                let (w, h, on) = state.device.preferred;
                out.extend([0, 0, w, h, on as u32, 0]);
                out.resize(wire::DISPLAY_INFO_BYTES as usize / 4, 0);
            } else if reply.answer == wire::RESP_OK_EDID {
                // §5.7.6.8's `size`, which QEMU answers with the buffer's 1024.
                out.extend([1024, 0]);
                out.extend(state.device.edid.chunks(4).map(|w| u32::from_le_bytes(w.try_into().unwrap())));
                out.resize(wire::EDID_BYTES as usize / 4, 0);
            }
            // A device writes no more than the room it was given.
            out.truncate(room as usize / 4);
            let bytes = (out.len() * 4) as u32;
            for (nth, word) in out.iter().enumerate() {
                self.mem.set((answer_addr - BASE) as usize + 4 * nth, 4, *word as u64);
            }
            reply.len.unwrap_or(bytes)
        };
        let (parts, size) = Self::parts(queue);
        let used = self.state.borrow().used[queue as usize];
        let at = parts.used + 4 + (used % size) as usize * 8;
        self.mem.set(at, 4, head as u64);
        self.mem.set(at + 4, 4, len as u64);
        let used = used.wrapping_add(1);
        self.state.borrow_mut().used[queue as usize] = used;
        self.mem.set(parts.used + 2, 2, used as u64);
    }

    fn trace(&self) -> Vec<Ev> {
        self.state.borrow().trace.clone()
    }

    fn forget(&self) {
        let mut state = self.state.borrow_mut();
        state.trace.clear();
        state.sent.clear();
    }

    fn sent(&self) -> Vec<Sent> {
        self.state.borrow().sent.clone()
    }

    fn with(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.borrow_mut());
    }
}

/// The holder: the model's doorbell, configuration, interrupt and lender.
struct Holder(Model);

impl Claim for Holder {
    type Backing = u64;
    type Refused = ();

    fn notify(&self, published: Published) {
        self.0.take(published.queue());
    }

    fn events_read(&mut self) -> Result<u32, toyos_virtio::pci::Refusal> {
        let mut state = self.0.state.borrow_mut();
        state.trace.push(Ev::EventsRead);
        Ok(state.device.events)
    }

    fn events_clear(&mut self, events: u32) -> Result<(), toyos_virtio::pci::Refusal> {
        let mut state = self.0.state.borrow_mut();
        state.trace.push(Ev::EventsClear(events));
        state.device.events &= !events;
        Ok(())
    }

    fn wait(&mut self) -> bool {
        let held = {
            let mut state = self.0.state.borrow_mut();
            state.trace.push(Ev::Wait);
            if state.silent {
                return false;
            }
            state.device.events |= core::mem::take(&mut state.device.raise_at_wait);
            core::mem::take(&mut state.held)
        };
        for (queue, head) in held {
            self.0.answer(queue, head);
        }
        true
    }

    fn lend(&mut self, bytes: u64) -> Result<(u64, u64), ()> {
        let mut state = self.0.state.borrow_mut();
        if state.refuse_lend {
            return Err(());
        }
        let addr = LENT + state.lent;
        state.lent += bytes.next_multiple_of(0x20_0000);
        state.trace.push(Ev::Lend(addr));
        Ok((addr, addr))
    }

    fn unlend(&mut self, backing: u64) {
        self.0.state.borrow_mut().trace.push(Ev::Unlend(backing));
    }
}

type Driver = Gpu<Mem, Holder>;

fn open(model: &Model) -> Driver {
    let queues = Queues::new(model.mem.clone());
    Gpu::open(queues, model.mem.clone(), Holder(model.clone()), true)
}

/// A driver with `mode` on screen, and the model's record forgotten.
fn showing(model: &Model, mode: (u32, u32)) -> Driver {
    let mut gpu = open(model);
    flip(&mut gpu, mode).expect("the model answers").expect("the model takes the mode");
    model.forget();
    gpu
}

fn flip(gpu: &mut Driver, mode: (u32, u32)) -> Result<Result<(), Declined<()>>, Refusal> {
    match gpu.prepare(mode)? {
        Ok(next) => gpu.show(next),
        Err(declined) => Ok(Err(declined)),
    }
}

const CREATE: u32 = 0x0101;
const UNREF: u32 = 0x0102;
const SET_SCANOUT: u32 = 0x0103;
const FLUSH: u32 = 0x0104;
const TRANSFER: u32 = 0x0105;
const ATTACH: u32 = 0x0106;
const DISPLAY_INFO: u32 = 0x0100;
const GET_EDID: u32 = 0x010a;
const UPDATE_CURSOR: u32 = 0x0300;
const MOVE_CURSOR: u32 = 0x0301;

// --- §5.7.4.2 and §5.7.5: the output, and its events ---

/// §5.7.5: the output is the display info's preferred size and the EDID, read
/// after the events are cleared.
#[test]
fn the_output_is_read_after_the_events_are_cleared() {
    let model = Model::new();
    model.with(|s| s.device.events = wire::EVENT_DISPLAY);
    let mut gpu = open(&model);
    let output = gpu.output().expect("the model answers");
    assert_eq!(
        model.trace(),
        [
            Ev::EventsRead,
            Ev::EventsClear(wire::EVENT_DISPLAY),
            Ev::Command(DISPLAY_INFO, 0),
            Ev::Command(GET_EDID, 0),
        ]
    );
    let mut block = [0u8; 128];
    block[..8].copy_from_slice(&[0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0]);
    block[127] = 0x42;
    assert_eq!(output, Output { preferred: Some((1280, 800)), edid: Some(block) });
    assert!(!gpu.display_changed());
}

/// An event the device raises while the driver reads the output is not
/// cleared by it: the next wake sees it.
#[test]
fn an_event_raised_during_the_reads_is_seen_on_the_next_wake() {
    let model = Model::answering(|sent, device| {
        if sent.code == DISPLAY_INFO {
            device.events |= wire::EVENT_DISPLAY;
        }
        answered(sent)
    });
    let mut gpu = open(&model);
    gpu.output().expect("the model answers");
    assert!(!gpu.display_changed());
    gpu.wake().expect("the model answers");
    assert!(gpu.display_changed(), "the event raised between the clear and the reads was lost");
}

#[test]
fn a_disabled_scanout_has_no_preferred_size_and_no_edid_is_asked_without_the_feature() {
    let model = Model::new();
    model.with(|s| s.device.preferred = (1024, 768, false));
    let queues = Queues::new(model.mem.clone());
    let mut gpu = Gpu::open(queues, model.mem.clone(), Holder(model.clone()), false);
    assert_eq!(gpu.output(), Ok(Output { preferred: None, edid: None }));
    assert!(!model.trace().contains(&Ev::Command(GET_EDID, 0)));
}

/// One interrupt serves the queues and the configuration, so an event that
/// lands while a frame's transfers are awaited is noted by that wait.
#[test]
fn an_event_raised_while_a_frames_transfer_is_awaited_is_read_on_the_next_pass() {
    let model = Model::new();
    let mut gpu = showing(&model, (1280, 800));
    model.with(|s| {
        s.deferred = true;
        s.device.raise_at_wait = wire::EVENT_DISPLAY;
    });
    gpu.present(&[Rect { x: 0, y: 0, width: 16, height: 16 }]).expect("the model answers");
    assert!(gpu.display_changed(), "a display event the frame's wait took was lost");
}

// --- §5.7.6.5 and §5.7.6.7: fences and answers ---

/// Every control command carries the fence flag and an id of its own; no
/// cursor command does.
#[test]
fn every_control_command_is_fenced_with_its_own_id() {
    let model = Model::new();
    let mut gpu = open(&model);
    gpu.output().expect("the model answers");
    flip(&mut gpu, (1280, 800)).expect("the model answers").expect("taken");
    flip(&mut gpu, (1600, 900)).expect("the model answers").expect("taken");
    gpu.present(&[Rect { x: 1, y: 2, width: 3, height: 4 }]).expect("the model answers");
    gpu.cursor(&[0; 4096], (0, 0), (5, 5)).expect("the model answers");
    gpu.move_cursor((6, 6)).expect("the model answers");
    let sent = model.sent();
    let control: Vec<&Sent> = sent.iter().filter(|s| s.queue == wire::CONTROL_QUEUE).collect();
    assert!(control.len() > 10);
    assert!(control.iter().all(|s| s.flags == wire::FLAG_FENCE), "{control:?}");
    let fences: Vec<u64> = control.iter().map(|s| s.fence).collect();
    assert!(fences.windows(2).all(|w| w[0] < w[1]), "fences repeat: {fences:?}");
    let cursor: Vec<&Sent> = sent.iter().filter(|s| s.queue == wire::CURSOR_QUEUE).collect();
    assert_eq!(cursor.iter().map(|s| s.code).collect::<Vec<_>>(), [UPDATE_CURSOR, MOVE_CURSOR]);
    assert!(cursor.iter().all(|s| s.flags == 0));
}

#[test]
fn an_answer_without_the_fence_or_with_another_id_is_refused() {
    let model = Model::answering(|sent, _| Reply { flags: Some(0), ..answered(sent) });
    let mut gpu = open(&model);
    assert!(matches!(
        gpu.output(),
        Err(Refusal::Unfenced { command: Command::GetDisplayInfo, flags: 0, .. })
    ));

    let model = Model::answering(|sent, _| Reply { fence: Some(sent.fence + 1), ..answered(sent) });
    let mut gpu = open(&model);
    assert_eq!(
        gpu.output(),
        Err(Refusal::Unfenced { command: Command::GetDisplayInfo, flags: wire::FLAG_FENCE, fence: 2, sent: 1 })
    );
}

#[test]
fn an_answer_shorter_than_its_structure_is_refused() {
    let model = Model::answering(|sent, _| Reply { len: Some(wire::HDR_BYTES), ..answered(sent) });
    let mut gpu = open(&model);
    assert_eq!(
        gpu.output(),
        Err(Refusal::ShortAnswer {
            command: Command::GetDisplayInfo,
            written: wire::HDR_BYTES,
            wanted: wire::DISPLAY_INFO_BYTES
        })
    );
    let model = Model::answering(|sent, _| Reply { len: Some(8), ..answered(sent) });
    let mut gpu = open(&model);
    assert_eq!(
        flip(&mut gpu, (800, 600)),
        Err(Refusal::ShortAnswer { command: Command::ResourceCreate2d, written: 8, wanted: wire::HDR_BYTES })
    );
}

#[test]
fn a_device_that_never_answers_is_refused_by_the_command_it_owes() {
    let model = Model::new();
    let mut gpu = showing(&model, (800, 600));
    model.with(|s| {
        s.deferred = true;
        s.silent = true;
    });
    assert_eq!(
        gpu.present(&[Rect::whole(800, 600)]),
        Err(Refusal::Silent(Command::TransferToHost2d))
    );
}

// --- §5.7.6.3: the page flip ---

/// The new scanout is made and filled while the old one is on screen, and the
/// old one's backing is given back only once its `UNREF` is answered, after
/// `SET_SCANOUT`.
#[test]
fn a_flip_unlends_the_old_backing_only_after_set_scanout_and_its_unref() {
    let model = Model::new();
    let mut gpu = showing(&model, (1280, 800));
    let old = *gpu.backing().expect("on screen");
    let next = gpu.prepare((1600, 900)).expect("the model answers").expect("taken");
    let new = *next.backing();
    assert_eq!(gpu.mode(), Some((1280, 800)), "the old mode stays until the new one is shown");
    gpu.show(next).expect("the model answers").expect("taken");
    assert_eq!(gpu.mode(), Some((1600, 900)));
    assert_eq!(gpu.backing(), Some(&new));
    assert_eq!(
        model.trace(),
        [
            Ev::Lend(new),
            Ev::Command(CREATE, 3),
            Ev::Command(ATTACH, 3),
            Ev::Command(TRANSFER, 3),
            Ev::Command(SET_SCANOUT, 3),
            Ev::Command(FLUSH, 3),
            Ev::Command(UNREF, 2),
            Ev::Unlend(old),
        ]
    );
    let sent = model.sent();
    assert_eq!(sent[1].payload, [3, 1, new as u32, (new >> 32) as u32, 1600 * 900 * 4, 0]);
    assert_eq!(sent[2].payload, [0, 0, 1600, 900, 0, 0, 3, 0]);
    assert_eq!(sent[3].payload, [0, 0, 1600, 900, 0, 3]);
}

/// §5.7.6.7's `RESP_ERR_OUT_OF_MEMORY` at each of the three steps that make
/// a scanout declines the mode: the old one stays, and the new one's
/// resource and backing are given back.
#[test]
fn out_of_memory_at_each_step_keeps_the_old_mode() {
    for (refused, command, after) in [
        (CREATE, Command::ResourceCreate2d, &[][..]),
        (ATTACH, Command::ResourceAttachBacking, &[UNREF][..]),
        (SET_SCANOUT, Command::SetScanout, &[UNREF][..]),
    ] {
        let model = Model::answering(move |sent, _| match (sent.code, sent.resource()) {
            (code, 3) if code == refused => Reply { answer: wire::RESP_ERR_OUT_OF_MEMORY, ..ok() },
            _ => answered(sent),
        });
        let mut gpu = showing(&model, (1280, 800));
        let old = *gpu.backing().expect("on screen");
        assert_eq!(flip(&mut gpu, (1600, 900)), Ok(Err(Declined::Device(command))), "{}", command.name());
        assert_eq!(gpu.mode(), Some((1280, 800)));
        assert_eq!(gpu.backing(), Some(&old));
        let trace = model.trace();
        let lent = match trace[0] {
            Ev::Lend(at) => at,
            ref other => panic!("{}: the flip began with {other:?}", command.name()),
        };
        let at = trace.iter().position(|e| *e == Ev::Command(refused, 3)).expect("the refused command was sent");
        let tail: Vec<Ev> = after.iter().map(|code| Ev::Command(*code, 3)).chain([Ev::Unlend(lent)]).collect();
        assert_eq!(trace[at + 1..], tail[..], "{}", command.name());
        assert!(!trace.contains(&Ev::Unlend(old)));
    }
}

#[test]
fn a_lend_refused_declines_the_mode_before_the_device_hears_of_it() {
    let model = Model::new();
    let mut gpu = showing(&model, (1280, 800));
    model.with(|s| s.refuse_lend = true);
    assert_eq!(flip(&mut gpu, (1600, 900)), Ok(Err(Declined::Lend(()))));
    assert!(model.trace().is_empty());
    assert_eq!(gpu.mode(), Some((1280, 800)));
}

/// Every other error answer, to any command, is a fault and ends the device.
#[test]
fn any_other_error_answer_is_refused_by_name() {
    let model = Model::answering(|sent, _| match sent.code {
        CREATE => Reply { answer: wire::RESP_ERR_INVALID_PARAMETER, ..ok() },
        _ => answered(sent),
    });
    let mut gpu = open(&model);
    assert_eq!(
        flip(&mut gpu, (800, 600)),
        Err(Refusal::Answered { command: Command::ResourceCreate2d, answer: wire::RESP_ERR_INVALID_PARAMETER })
    );

    let model = Model::new();
    let mut gpu = showing(&model, (800, 600));
    model.with(|s| s.answer = Box::new(|sent, _| match sent.code {
        TRANSFER => Reply { answer: wire::RESP_ERR_OUT_OF_MEMORY, ..ok() },
        _ => answered(sent),
    }));
    assert_eq!(
        gpu.present(&[Rect::whole(10, 10)]),
        Err(Refusal::Answered { command: Command::TransferToHost2d, answer: wire::RESP_ERR_OUT_OF_MEMORY })
    );

    let model = Model::new();
    let mut gpu = showing(&model, (800, 600));
    model.with(|s| s.answer = Box::new(|sent, _| match sent.code {
        UNREF => Reply { answer: wire::RESP_ERR_INVALID_RESOURCE_ID, ..ok() },
        _ => answered(sent),
    }));
    assert_eq!(
        flip(&mut gpu, (1024, 768)),
        Err(Refusal::Answered { command: Command::ResourceUnref, answer: wire::RESP_ERR_INVALID_RESOURCE_ID })
    );

    let model = Model::answering(|sent, _| Reply { answer: wire::RESP_OK_EDID, ..answered(sent) });
    let mut gpu = open(&model);
    assert_eq!(
        gpu.output(),
        Err(Refusal::Answered { command: Command::GetDisplayInfo, answer: wire::RESP_OK_EDID })
    );
}

// --- §5.7.6.2: a present ---

/// Every rectangle's transfer is answered before any flush is sent, each at
/// the offset of its first byte in a row-major backing.
#[test]
fn a_present_transfers_every_rectangle_before_it_flushes_any() {
    let model = Model::new();
    let mut gpu = showing(&model, (1280, 800));
    let rects = [Rect { x: 10, y: 20, width: 30, height: 40 }, Rect { x: 0, y: 799, width: 1280, height: 1 }];
    gpu.present(&rects).expect("the model answers");
    let codes: Vec<u32> = model.sent().iter().map(|s| s.code).collect();
    assert_eq!(codes, [TRANSFER, TRANSFER, FLUSH, FLUSH]);
    let offset = |x: u64, y: u64| (y * 1280 + x) * 4;
    assert_eq!(model.sent()[0].payload, [10, 20, 30, 40, offset(10, 20) as u32, 0, 2, 0]);
    assert_eq!(model.sent()[1].payload, [0, 799, 1280, 1, offset(0, 799) as u32, 0, 2, 0]);
    assert_eq!(model.sent()[3].payload, [0, 799, 1280, 1, 2, 0]);
}

/// More rectangles than control slots: a slot is reused once its command is
/// answered.
#[test]
fn a_present_wider_than_the_queue_waits_for_slots() {
    let model = Model::new();
    let mut gpu = showing(&model, (1280, 800));
    model.with(|s| s.deferred = true);
    let rects: Vec<Rect> = (0..20).map(|y| Rect { x: 0, y, width: 8, height: 1 }).collect();
    gpu.present(&rects).expect("the model answers");
    assert_eq!(model.sent().len(), 40);
    assert!(model.trace().contains(&Ev::Wait));
}

#[test]
#[should_panic(expected = "outside the 1280x800 scanout")]
fn a_rectangle_outside_the_scanout_is_the_callers_mistake() {
    let model = Model::new();
    let mut gpu = showing(&model, (1280, 800));
    let _ = gpu.present(&[Rect { x: 1270, y: 0, width: 11, height: 1 }]);
}

// --- §5.7.6.6 and §5.7.6.10: the cursor ---

/// The cursor's resource is made once, its image transferred and answered
/// before `UPDATE_CURSOR`, and QEMU's cursor queue answers nothing (used
/// `len` 0), which is no answer.
#[test]
fn the_cursor_is_filled_and_fenced_before_it_is_shown() {
    let model = Model::new();
    let mut gpu = showing(&model, (800, 600));
    let mut image = [0u32; 4096];
    image[0] = 0x8011_2233;
    gpu.cursor(&image, (3, 4), (100, 200)).expect("the model answers");
    gpu.cursor(&image, (3, 4), (100, 200)).expect("the model answers");
    let codes: Vec<u32> = model.sent().iter().map(|s| s.code).collect();
    assert_eq!(codes, [CREATE, ATTACH, TRANSFER, UPDATE_CURSOR, TRANSFER, UPDATE_CURSOR]);
    assert_eq!(model.sent()[0].payload, [1, wire::FORMAT_B8G8R8A8_UNORM, 64, 64]);
    assert_eq!(model.sent()[3].payload, [0, 100, 200, 0, 1, 3, 4, 0]);
    let attach = &model.sent()[1].payload;
    let image_at = u64::from(attach[2]) | u64::from(attach[3]) << 32;
    assert_eq!(model.mem.words_at(image_at, 4), [0x8011_2233]);
}

#[test]
fn a_cursor_answer_is_read_only_at_a_headers_length() {
    for (answer, want) in [
        (Some((wire::HDR_BYTES, wire::RESP_OK_NODATA)), Ok(())),
        (
            Some((wire::HDR_BYTES, wire::RESP_ERR_INVALID_RESOURCE_ID)),
            Err(Refusal::Answered { command: Command::MoveCursor, answer: wire::RESP_ERR_INVALID_RESOURCE_ID }),
        ),
        (Some((8, wire::RESP_OK_NODATA)), Err(Refusal::CursorAnswer { command: Command::MoveCursor, written: 8 })),
    ] {
        let model = Model::new();
        let mut gpu = showing(&model, (800, 600));
        model.with(|s| s.cursor_answer = answer);
        gpu.move_cursor((1, 1)).expect("published");
        assert_eq!(gpu.wake(), want, "{answer:?}");
    }
    // Past the header's 24 bytes is more than the chain may be written.
    let model = Model::new();
    let mut gpu = showing(&model, (800, 600));
    model.with(|s| s.cursor_answer = Some((25, wire::RESP_OK_NODATA)));
    gpu.move_cursor((1, 1)).expect("published");
    assert!(matches!(gpu.wake(), Err(Refusal::Used(wire::CURSOR_QUEUE, UsedRefusal::Written { .. }))));
}

/// A cursor queue whose slots are all the device's waits for one.
#[test]
fn a_full_cursor_queue_waits_for_a_slot() {
    let model = Model::new();
    let mut gpu = showing(&model, (800, 600));
    model.with(|s| s.deferred = true);
    for x in 0..9 {
        gpu.move_cursor((x, 0)).expect("the model answers");
    }
    assert_eq!(model.trace().iter().filter(|e| **e == Ev::Wait).count(), 1);
    assert_eq!(model.sent().len(), 9);
}
