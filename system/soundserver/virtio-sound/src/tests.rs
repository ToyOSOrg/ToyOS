//! The driver against a model of the device that answers on the doorbell,
//! with §5.14's tables as the oracle for every byte it is sent and every
//! answer a device can lie in.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use toyos_device_memory::DmaBuffers;
use toyos_virtio::queue::{Published, UsedRefusal};

use crate::wire::{self, Event, Failed, PcmInfo, Request};
use crate::{
    Doorbell, Queues, Refusal, Sound, Stream, CONTROL_PARTS, EVENTS, EVENT_PARTS, PERIODS, PERIOD_BYTES,
    TX_PARTS,
};

/// Where the device reaches the grant's first byte.
const BASE: u64 = 0x4000_0000;

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

    fn bytes_at(&self, addr: u64, len: u32) -> Vec<u8> {
        let at = (addr - BASE) as usize;
        self.0.borrow()[at..at + len as usize].to_vec()
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

/// One element of a chain the device took: §2.7.5's descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Desc {
    addr: u64,
    len: u32,
    writable: bool,
}

/// What the model answers a control request with.
struct Answer {
    status: u32,
    payload: Vec<u32>,
    /// The `len` it reports, where it is not the bytes it wrote.
    len: Option<u32>,
    silent: bool,
}

fn ok() -> Answer {
    Answer { status: wire::S_OK, payload: Vec::new(), len: None, silent: false }
}

/// QEMU's stream at v11.1.1: S16 at 44100 and 48000 among others, output,
/// one or two channels.
fn qemu_stream() -> PcmInfo {
    PcmInfo {
        hda_fn_nid: 0,
        features: 0,
        formats: 1 << wire::FMT_S16 | 1 << 3,
        rates: 1 << wire::RATE_44100 | 1 << wire::RATE_48000 | 1 << 1,
        direction: wire::D_OUTPUT,
        channels_min: 1,
        channels_max: 2,
    }
}

/// `info` as the eight words of §5.14.6.6.2's structure.
fn encode(info: PcmInfo) -> Vec<u32> {
    let mut bytes = [0u8; 32];
    bytes[0..4].copy_from_slice(&info.hda_fn_nid.to_le_bytes());
    bytes[4..8].copy_from_slice(&info.features.to_le_bytes());
    bytes[8..16].copy_from_slice(&info.formats.to_le_bytes());
    bytes[16..24].copy_from_slice(&info.rates.to_le_bytes());
    bytes[24] = info.direction;
    bytes[25] = info.channels_min;
    bytes[26] = info.channels_max;
    words(&bytes)
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes.chunks(4).map(|w| u32::from_le_bytes(w.try_into().unwrap())).collect()
}

/// How the model answers each control request it reads.
type Answering = Box<dyn FnMut(&[u32]) -> Answer>;

struct State {
    /// Per queue: the available entries taken so far, and the used ones
    /// written.
    taken: [u16; 3],
    used: [u16; 3],
    /// Every control request, as the device read it.
    requests: Vec<Vec<u8>>,
    /// Every chain the device took, by queue, in order.
    chains: [Vec<(u16, Vec<Desc>)>; 3],
    /// Chains the device holds: the transmit queue's until played, the
    /// event queue's until an event.
    held: [Vec<(u16, Vec<Desc>)>; 3],
    answer: Answering,
}

#[derive(Clone)]
struct Model {
    mem: Mem,
    state: Rc<RefCell<State>>,
}

impl Model {
    fn new() -> Self {
        Self::answering(|request| match request[0] {
            wire::R_PCM_INFO => Answer { payload: encode(qemu_stream()), ..ok() },
            _ => ok(),
        })
    }

    fn answering(answer: impl FnMut(&[u32]) -> Answer + 'static) -> Self {
        Self {
            mem: Mem::new(),
            state: Rc::new(RefCell::new(State {
                taken: [0; 3],
                used: [0; 3],
                requests: Vec::new(),
                chains: Default::default(),
                held: Default::default(),
                answer: Box::new(answer),
            })),
        }
    }

    fn parts(queue: u16) -> (crate::Parts, u16) {
        match queue {
            wire::CONTROL_QUEUE => (CONTROL_PARTS, crate::CONTROL_QUEUE_SIZE),
            wire::EVENT_QUEUE => (EVENT_PARTS, crate::EVENT_QUEUE_SIZE),
            wire::TX_QUEUE => (TX_PARTS, crate::TX_QUEUE_SIZE),
            other => panic!("model: queue {other} was never configured"),
        }
    }

    /// The chain at `head`, by following its descriptors (§2.7.5).
    fn chain(&self, queue: u16, head: u16) -> Vec<Desc> {
        let (parts, size) = Self::parts(queue);
        let mut descs = Vec::new();
        let mut at = head;
        loop {
            assert!(at < size && descs.len() < size as usize, "model: a chain off the table");
            let d = parts.desc + at as usize * 16;
            let flags = self.mem.get(d + 12, 2);
            descs.push(Desc {
                addr: self.mem.get(d, 8),
                len: self.mem.get(d + 8, 4) as u32,
                writable: flags & 2 != 0,
            });
            if flags & 1 == 0 {
                return descs;
            }
            at = self.mem.get(d + 14, 2) as u16;
        }
    }

    /// A used element on `queue`: `head`, `len` bytes written (§2.7.8).
    fn give_back(&self, queue: u16, head: u16, len: u32) {
        let (parts, size) = Self::parts(queue);
        let mut state = self.state.borrow_mut();
        let used = &mut state.used[queue as usize];
        let element = parts.used + 4 + (*used % size) as usize * 8;
        self.mem.set(element, 4, head as u64);
        self.mem.set(element + 4, 4, len as u64);
        *used = used.wrapping_add(1);
        self.mem.set(parts.used + 2, 2, *used as u64);
    }

    /// Play the `count` oldest periods: each status `status`, `len` reported.
    fn play_with(&self, count: usize, status: u32, len: u32) {
        for _ in 0..count {
            let (head, chain) = self.state.borrow_mut().held[wire::TX_QUEUE as usize].remove(0);
            self.mem.set((chain[2].addr - BASE) as usize, 4, status as u64);
            self.give_back(wire::TX_QUEUE, head, len);
        }
    }

    fn play(&self, count: usize) {
        self.play_with(count, wire::S_OK, wire::STATUS_BYTES);
    }

    /// Write `event` into the oldest event buffer, `len` reported.
    fn event(&self, event: Event, len: u32) {
        let (head, chain) = self.state.borrow_mut().held[wire::EVENT_QUEUE as usize].remove(0);
        let at = (chain[0].addr - BASE) as usize;
        self.mem.set(at, 4, event.code as u64);
        self.mem.set(at + 4, 4, event.data as u64);
        self.give_back(wire::EVENT_QUEUE, head, len);
    }

    /// The control requests' codes, in the order the device read them.
    fn codes(&self) -> Vec<u32> {
        self.state.borrow().requests.iter().map(|r| words(&r[..4])[0]).collect()
    }

    fn chains(&self, queue: u16) -> Vec<(u16, Vec<Desc>)> {
        self.state.borrow().chains[queue as usize].clone()
    }
}

impl Doorbell for Model {
    fn ring(&self, published: Published) {
        let queue = published.queue();
        let (parts, size) = Self::parts(queue);
        loop {
            let available = self.mem.get(parts.avail + 2, 2) as u16;
            let taken = self.state.borrow().taken[queue as usize];
            if taken == available {
                return;
            }
            let head = self.mem.get(parts.avail + 4 + (taken % size) as usize * 2, 2) as u16;
            self.state.borrow_mut().taken[queue as usize] = taken.wrapping_add(1);
            let chain = self.chain(queue, head);
            self.state.borrow_mut().chains[queue as usize].push((head, chain.clone()));
            if queue != wire::CONTROL_QUEUE {
                self.state.borrow_mut().held[queue as usize].push((head, chain));
                continue;
            }
            let request: Vec<u8> = chain
                .iter()
                .filter(|d| !d.writable)
                .flat_map(|d| self.mem.bytes_at(d.addr, d.len))
                .collect();
            self.state.borrow_mut().requests.push(request.clone());
            let answer = (self.state.borrow_mut().answer)(&words(&request));
            if answer.silent {
                continue;
            }
            let response = chain.iter().find(|d| d.writable).expect("a response buffer");
            let mut out = vec![answer.status];
            out.extend(&answer.payload);
            let wrote = (4 * out.len() as u32).min(response.len);
            for (nth, word) in out.iter().enumerate().take(wrote as usize / 4) {
                self.mem.set((response.addr - BASE) as usize + 4 * nth, 4, *word as u64);
            }
            self.give_back(queue, head, answer.len.unwrap_or(wrote));
        }
    }
}

fn open(model: &Model, streams: u32) -> Result<(Sound<Mem, Model>, Stream), Refusal> {
    Sound::open(Queues::new(model.mem.clone()), model.mem.clone(), model.clone(), streams)
}

fn opened(model: &Model) -> Sound<Mem, Model> {
    open(model, 1).unwrap_or_else(|why| panic!("the model's device opens: {why}")).0
}

// --- §5.14.6: the messages, byte for byte ---

/// §5.14.6.1 and §5.14.6.6.2: `virtio_snd_query_info` is `hdr`, `start_id`,
/// `count` and `size`, each `le32`; §5.14.6.6.3: `virtio_snd_pcm_set_params`
/// is `hdr`, `stream_id`, `buffer_bytes`, `period_bytes`, `features`, then
/// `channels`, `format`, `rate` and a padding byte; §5.14.6.6: a PREPARE is
/// `hdr` and `stream_id`. The bring-up is §5.14.5's order.
#[test]
fn the_bring_up_sends_the_specifications_bytes_in_its_order() {
    let model = Model::new();
    let (_, stream) = open(&model, 1).expect("the model's device opens");
    assert_eq!(stream.rate, 44100);
    assert_eq!(stream.channels, 2);
    assert_eq!(stream.info, qemu_stream());

    let requests = model.state.borrow().requests.clone();
    assert_eq!(
        requests,
        [
            vec![0x00, 0x01, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 32, 0, 0, 0],
            vec![
                0x01, 0x01, 0, 0, 0, 0, 0, 0, 0x00, 0x10, 0, 0, 0x00, 0x02, 0, 0, 0, 0, 0, 0, 2,
                5, 6, 0
            ],
            vec![0x02, 0x01, 0, 0, 0, 0, 0, 0],
        ]
    );
    // §5.14.6.2: a response buffer of `sizeof(hdr) + count * size`; a bare
    // status for the others.
    let responses: Vec<u32> = model
        .chains(wire::CONTROL_QUEUE)
        .iter()
        .map(|(_, chain)| {
            assert_eq!(chain.len(), 2);
            assert!(!chain[0].writable && chain[1].writable);
            chain[1].len
        })
        .collect();
    assert_eq!(responses, [36, 4, 4]);
    // §5.14.5.1: the event queue full of device-writable buffers of at least
    // `virtio_snd_event`'s eight bytes, and none device-readable.
    let events = model.chains(wire::EVENT_QUEUE);
    assert_eq!(events.len(), crate::EVENT_QUEUE_SIZE as usize);
    for (head, chain) in &events {
        assert_eq!(chain, &[Desc { addr: BASE + (EVENTS + *head as usize * 8) as u64, len: 8, writable: true }]);
    }
    assert!(model.chains(wire::TX_QUEUE).is_empty(), "a period before the stream is started");
}

/// §5.14.6.6.2's `virtio_snd_pcm_info`: `hdr` at 0, `features` at 4,
/// `formats` at 8 and `rates` at 16 as `le64`, then `direction`,
/// `channels_min` and `channels_max` at 24, 25 and 26.
#[test]
fn stream_information_is_decoded_at_the_specifications_offsets() {
    let mut bytes = [0u8; 32];
    bytes[0..4].copy_from_slice(&0x11u32.to_le_bytes());
    bytes[4..8].copy_from_slice(&0x2222_2222u32.to_le_bytes());
    bytes[8..16].copy_from_slice(&0x0807_0605_0403_0201u64.to_le_bytes());
    bytes[16..24].copy_from_slice(&0x1817_1615_1413_1211u64.to_le_bytes());
    bytes[24..27].copy_from_slice(&[1, 3, 7]);
    bytes[27..32].copy_from_slice(&[0xFF; 5]);
    let info = PcmInfo::decode(words(&bytes).try_into().unwrap());
    assert_eq!(
        info,
        PcmInfo {
            hda_fn_nid: 0x11,
            features: 0x2222_2222,
            formats: 0x0807_0605_0403_0201,
            rates: 0x1817_1615_1413_1211,
            direction: 1,
            channels_min: 3,
            channels_max: 7,
        }
    );
}

/// §5.14.6: the four status codes and the four event types.
#[test]
fn every_code_is_the_specifications_number() {
    assert_eq!(wire::status(0x8000), Ok(Ok(())));
    assert_eq!(wire::status(0x8001), Ok(Err(Failed::BadMsg)));
    assert_eq!(wire::status(0x8002), Ok(Err(Failed::NotSupp)));
    assert_eq!(wire::status(0x8003), Ok(Err(Failed::IoErr)));
    for none in [0, 0x7FFF, 0x8004, u32::MAX] {
        assert_eq!(wire::status(none), Err(none));
    }
    let named = |code| Event { code, data: 0 }.name();
    assert_eq!(named(0x1000), Some("jack connected"));
    assert_eq!(named(0x1001), Some("jack disconnected"));
    assert_eq!(named(0x1100), Some("period elapsed"));
    assert_eq!(named(0x1101), Some("PCM XRUN"));
    assert_eq!(named(0x1102), None);
    // §5.14.6.6.2's enumerations, by the bit each is.
    assert_eq!((wire::FMT_S16, wire::RATE_44100, wire::RATE_48000), (5, 6, 7));
}

// --- What the driver chooses ---

/// A rate and a channel count the stream offers: 44100 over 48000, stereo
/// over mono, and each refusal names the bitmap or range it read.
#[test]
fn the_stream_is_set_to_what_it_offers_or_refused_by_what_it_lacks() {
    let with = |change: fn(&mut PcmInfo)| {
        let mut info = qemu_stream();
        change(&mut info);
        let model = Model::answering(move |request| match request[0] {
            wire::R_PCM_INFO => Answer { payload: encode(info), ..ok() },
            _ => ok(),
        });
        let set = open(&model, 1).map(|(_, stream)| (stream.rate, stream.channels));
        (set, model)
    };
    assert_eq!(with(|i| i.rates = 1 << wire::RATE_48000).0, Ok((48000, 2)));
    assert_eq!(with(|i| i.channels_max = 1).0, Ok((44100, 1)));
    assert_eq!(with(|i| i.channels_max = 8).0, Ok((44100, 2)));
    let refused = [
        (with(|i| i.direction = 1), Refusal::NotOutput { direction: 1 }),
        (with(|i| i.formats = 1 << 3), Refusal::NoFormat { formats: 1 << 3 }),
        (with(|i| i.rates = 1 << 10), Refusal::NoRate { rates: 1 << 10 }),
        (with(|i| i.channels_min = 3), Refusal::NoChannels { min: 3, max: 2 }),
        (with(|i| i.channels_max = 0), Refusal::NoChannels { min: 1, max: 0 }),
    ];
    for ((set, model), refusal) in refused {
        assert_eq!(set, Err(refusal));
        // Nothing past the question: a stream it cannot carry is never set.
        assert_eq!(model.codes(), [wire::R_PCM_INFO]);
    }
}

// --- A device's answers, believed only as far as §5.14.6 goes ---

/// The negative control: each malformed answer is refused by its own name,
/// and the bring-up goes no further than the request it answered.
#[test]
fn a_malformed_control_answer_is_refused_by_name() {
    let answering = |answer: fn(&[u32]) -> Option<Answer>| {
        let model = Model::answering(move |request| {
            answer(request).unwrap_or_else(|| match request[0] {
                wire::R_PCM_INFO => Answer { payload: encode(qemu_stream()), ..ok() },
                _ => ok(),
            })
        });
        (open(&model, 1).err(), model.codes().len())
    };
    // A status §5.14.6 does not define.
    assert_eq!(
        answering(|r| (r[0] == wire::R_PCM_PREPARE).then(|| Answer { status: 0x1234, ..ok() })),
        (Some(Refusal::UnknownStatus(Request::Prepare, 0x1234)), 3)
    );
    // An `S_OK` with no stream information behind it.
    assert_eq!(
        answering(|r| (r[0] == wire::R_PCM_INFO).then(ok)),
        (Some(Refusal::ShortAnswer { request: Request::PcmInfo, written: 4, wanted: 36 }), 1)
    );
    // A `len` that covers less than the status word.
    assert_eq!(
        answering(|r| (r[0] == wire::R_PCM_PREPARE).then(|| Answer { len: Some(3), ..ok() })),
        (Some(Refusal::ShortAnswer { request: Request::Prepare, written: 3, wanted: 4 }), 3)
    );
    // A `len` past the response buffer is `toyos-virtio`'s to refuse.
    assert!(matches!(
        answering(|r| (r[0] == wire::R_PCM_SET_PARAMS).then(|| Answer { len: Some(5), ..ok() })),
        (Some(Refusal::Used(wire::CONTROL_QUEUE, UsedRefusal::Written { head: 0, .. })), 2)
    ));
    // A well-formed no.
    assert_eq!(
        answering(|r| (r[0] == wire::R_PCM_SET_PARAMS).then(|| Answer { status: wire::S_NOT_SUPP, ..ok() })),
        (Some(Refusal::Rejected(Request::SetParams, Failed::NotSupp)), 2)
    );
}

/// A device that never answers is called silent after a bounded count of
/// looks, and a device with no stream is never asked anything.
#[test]
fn a_silent_device_and_a_streamless_one_are_refused() {
    let model = Model::answering(|_| Answer { silent: true, ..ok() });
    assert_eq!(open(&model, 1).err(), Some(Refusal::Silent(Request::PcmInfo)));

    let model = Model::new();
    assert_eq!(open(&model, 0).err(), Some(Refusal::NoStream));
    assert!(model.codes().is_empty() && model.chains(wire::EVENT_QUEUE).is_empty());
}

// --- §5.14.6.8: the periods ---

/// The stream is started once before its first period, every period is
/// §5.14.6.8's three parts — the stream id, the PCM, a status for the device
/// — and a completion is the set of periods given back with `S_OK`.
#[test]
fn periods_go_out_after_one_start_and_come_back_as_a_mask() {
    let model = Model::new();
    let mut sound = opened(&model);
    for idx in 0..PERIODS {
        sound.submit(idx).expect("a period goes out");
    }
    assert_eq!(model.codes(), [wire::R_PCM_INFO, wire::R_PCM_SET_PARAMS, wire::R_PCM_PREPARE, wire::R_PCM_START]);
    let chains = model.chains(wire::TX_QUEUE);
    assert_eq!(chains.len(), PERIODS);
    for (idx, (head, chain)) in chains.iter().enumerate() {
        assert_eq!(*head as usize, idx * 3);
        assert_eq!(chain.len(), 3);
        assert_eq!((chain[0].len, chain[0].writable), (wire::XFER_BYTES, false));
        assert_eq!(model.mem.bytes_at(chain[0].addr, 4), [0, 0, 0, 0], "stream 0");
        assert_eq!(chain[1], Desc { addr: BASE + (idx * PERIOD_BYTES) as u64, len: 512, writable: false });
        assert_eq!((chain[2].len, chain[2].writable), (wire::STATUS_BYTES, true));
    }
    assert_eq!(sound.completed(), Ok(0));
    model.play(3);
    assert_eq!(sound.completed(), Ok(0b111));
    model.play(5);
    assert_eq!(sound.completed(), Ok(0b1111_1000));

    // A drained stream stops once, and starts again on its next period.
    sound.stop().expect("STOP");
    sound.stop().expect("nothing");
    sound.submit(4).expect("a period goes out");
    assert_eq!(
        model.codes()[3..],
        [wire::R_PCM_START, wire::R_PCM_STOP, wire::R_PCM_START]
    );
}

/// A period the device gives back with a short status, a failure, or a
/// status §5.14.6.8 does not define is refused by that name, as is a used
/// element for no period in flight.
#[test]
fn a_period_given_back_wrongly_is_refused_by_name() {
    let given_back = |status: u32, len: u32| {
        let model = Model::new();
        let mut sound = opened(&model);
        sound.submit(0).expect("a period goes out");
        sound.submit(1).expect("a period goes out");
        model.play(1);
        model.play_with(1, status, len);
        sound.completed()
    };
    assert_eq!(given_back(wire::S_OK, 8), Ok(0b11));
    assert_eq!(given_back(wire::S_OK, 4), Err(Refusal::ShortStatus { period: 1, written: 4 }));
    assert_eq!(
        given_back(wire::S_IO_ERR, 8),
        Err(Refusal::PeriodFailed { period: 1, status: Ok(Failed::IoErr) })
    );
    assert_eq!(given_back(7, 8), Err(Refusal::PeriodFailed { period: 1, status: Err(7) }));

    let model = Model::new();
    let mut sound = opened(&model);
    sound.submit(0).expect("a period goes out");
    model.give_back(wire::TX_QUEUE, 3, 8);
    assert_eq!(
        sound.completed(),
        Err(Refusal::Used(wire::TX_QUEUE, UsedRefusal::NoChain { head: 3 }))
    );
}

/// An event is read, handed up and its buffer posted again; one shorter than
/// `virtio_snd_event` is refused and not read.
#[test]
fn an_event_is_handed_up_and_its_buffer_posted_again() {
    let model = Model::new();
    let mut sound = opened(&model);
    model.event(Event { code: wire::EVT_PCM_XRUN, data: 0 }, 8);
    let mut said = Vec::new();
    sound.events(|event| said.push(event)).expect("a whole event");
    assert_eq!(said, [Event { code: wire::EVT_PCM_XRUN, data: 0 }]);
    assert_eq!(model.chains(wire::EVENT_QUEUE).len(), crate::EVENT_QUEUE_SIZE as usize + 1);

    model.event(Event { code: wire::EVT_JACK_CONNECTED, data: 1 }, 4);
    let mut said = Vec::new();
    assert_eq!(sound.events(|event| said.push(event)), Err(Refusal::ShortEvent { written: 4 }));
    assert!(said.is_empty());
}

/// The claim's interrupt record, as [`Sound::played`] is handed it: when the
/// oldest and newest notification since the last take landed.
#[derive(Default)]
struct Claim(Option<(u64, u64)>);

impl Claim {
    fn land(&mut self, at: u64) {
        let first = self.0.map_or(at, |(first, _)| first);
        self.0 = Some((first, at));
    }

    fn take(&mut self) -> Option<(u64, u64)> {
        self.0.take()
    }
}

/// No period is stranded: every one comes back exactly once, and none while
/// the claim is left unreadable.
///
/// Two periods, each a used element and then its notification (§2.7.7),
/// against two wakes of a driver that takes the record and then reads the
/// ring, in every interleaving; then the driver wakes for as long as the
/// claim reads ready, which is all a waiting soundserver does. A ring read
/// with no record that took what it found would leave a period nobody's
/// notification answers for, and the claim would never wake the driver for
/// it again.
#[test]
fn no_period_is_stranded_between_its_used_element_and_its_notification() {
    #[derive(Clone, Copy)]
    enum Step {
        Used,
        Notified,
        Take,
        Read,
    }
    const DEVICE: [Step; 4] = [Step::Used, Step::Notified, Step::Used, Step::Notified];
    const DRIVER: [Step; 4] = [Step::Take, Step::Read, Step::Take, Step::Read];

    let mut orders = 0;
    // Bit `n` of `pick` set: the `n`th step of the merge is the device's.
    for pick in 0u32..1 << 8 {
        if pick.count_ones() != 4 {
            continue;
        }
        orders += 1;
        let model = Model::new();
        let mut sound = opened(&model);
        sound.submit(0).expect("a period goes out");
        sound.submit(1).expect("a period goes out");
        let mut claim = Claim::default();
        let (mut device, mut driver, mut landed) = (DEVICE.iter(), DRIVER.iter(), 0);
        let mut taken = None;
        let mut back = Vec::new();
        for n in 0..8 {
            let step = if pick & 1 << n != 0 { device.next() } else { driver.next() };
            match step.copied().expect("four steps each") {
                Step::Used => model.play(1),
                Step::Notified => {
                    landed += 1;
                    claim.land(landed);
                }
                Step::Take => taken = claim.take(),
                Step::Read => back.extend(sound.played(taken.take()).expect("a whole period")),
            }
        }
        while let Some(record) = claim.take() {
            back.extend(sound.played(Some(record)).expect("a whole period"));
        }
        let masks: Vec<u32> = back.iter().map(|(mask, _)| *mask).collect();
        let mut seen = 0;
        for mask in &masks {
            assert_eq!(seen & mask, 0, "order {pick:#010b}: a period came back twice in {masks:?}");
            seen |= mask;
        }
        assert_eq!(seen, 0b11, "order {pick:#010b}: the periods that came back are {masks:?}");
    }
    assert_eq!(orders, 70);
}
