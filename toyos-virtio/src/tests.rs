//! The transport and the queue against [`crate::stub`]: the sequences the
//! specification orders, and every word a device can lie in.

use std::collections::BTreeMap;
use std::vec::Vec;

use toyos_untrusted::Refused;

use crate::pci::{
    status, Layout, Live, Offer, Refusal, Setup, Source, VendorCap, WalkRefusal,
    VIRTIO_F_ACCESS_PLATFORM, VIRTIO_F_VERSION_1,
};
use crate::queue::{desc_bytes, Buffer, Parts, Used, UsedRefusal, Virtqueue};
use crate::stub::{
    Event, Machine, Ring, BAR_BYTES, COMMON_AT, DEVICE_AT, DEVICE_BASE, FEATURE_OFFERED, FEATURE_WITHHELD,
    GRANT_BYTES, NOTIFY_AT, NOTIFY_OFF_MULTIPLIER, QUEUES,
};

const SIZE: u16 = 8;
/// The MSI-X table entry every source is mapped to.
const ENTRY: u16 = 0;
/// Where the buffers a chain names are: past every queue's rings.
const BUFFERS: u64 = DEVICE_BASE + 0x2000;

fn parts(index: u16) -> Parts {
    Parts::contiguous(index as usize * 0x400, SIZE)
}

fn queue(machine: &Machine, index: u16) -> Virtqueue<Machine> {
    Virtqueue::new(machine.clone(), index, SIZE, parts(index))
}

fn ring(machine: &Machine, index: u16) -> Ring {
    machine.ring(parts(index), SIZE)
}

fn layout(machine: &Machine) -> Layout {
    Layout::of(&machine.caps()).expect("the stub's own capabilities")
}

fn setup(machine: &Machine) -> Setup<Machine> {
    Offer::acknowledge(machine.clone(), &layout(machine))
        .and_then(|offer| offer.accept(FEATURE_OFFERED))
        .expect("the stub negotiates")
}

/// A live device with queue 0 enabled.
fn live(machine: &Machine) -> (Live<Machine>, Virtqueue<Machine>) {
    let queue = queue(machine, 0);
    let setup = setup(machine)
        .config_vector(ENTRY)
        .and_then(|setup| setup.enable(&queue, ENTRY))
        .unwrap_or_else(|why| panic!("the stub takes a vector and queue 0: {why}"));
    (setup.driver_ok(), queue)
}

fn one(len: u32, writable: bool) -> [Buffer; 1] {
    [Buffer { addr: BUFFERS, len, writable }]
}

fn cap_of(caps: &mut [VendorCap], cfg_type: u8) -> &mut VendorCap {
    caps.iter_mut().find(|cap| cap.cfg_type == cfg_type).expect("the stub publishes it")
}

// --- §4.1.4: where the structures are ---

/// §4.1.4.1: "The driver SHOULD use the first instance of each virtio
/// structure type", and "MUST ignore any vendor-specific capability structure
/// which has a reserved bar value".
#[test]
fn the_first_capability_of_each_type_in_a_real_bar_is_the_one_used() {
    let machine = Machine::new();
    let mut caps = machine.caps();
    let bar = caps[0].bar;
    // A common structure in a BAR that is none, ahead of the real one; and a
    // second one behind it, over the ISR structure nothing may touch.
    caps.insert(0, VendorCap { cfg_type: 1, bar: 6, offset: 0, length: 0x1000, notify_off_multiplier: 0 });
    caps.push(VendorCap { cfg_type: 1, bar, offset: 0, length: 0x1000, notify_off_multiplier: 0 });
    let layout = Layout::of(&caps).expect("the real capabilities are all there");
    assert_eq!(layout.bar(), bar);
    // The stub reds on an access outside the structure it published first.
    Offer::acknowledge(machine, &layout).expect("the first common structure answers");
}

#[test]
fn a_device_missing_a_structure_is_refused_by_its_name() {
    let machine = Machine::new();
    for (cfg_type, name) in [(1, "COMMON_CFG"), (2, "NOTIFY_CFG"), (3, "ISR_CFG"), (4, "DEVICE_CFG")] {
        let caps: Vec<VendorCap> =
            machine.caps().into_iter().filter(|cap| cap.cfg_type != cfg_type).collect();
        assert_eq!(Layout::of(&caps), Err(Refusal::MissingCap(name)));
        // And one that is only in a reserved BAR is as missing.
        let mut caps = machine.caps();
        cap_of(&mut caps, cfg_type).bar = 6;
        assert_eq!(Layout::of(&caps), Err(Refusal::MissingCap(name)));
    }
    assert_eq!(Layout::of(&[]), Err(Refusal::MissingCap("COMMON_CFG")));
}

#[test]
fn structures_in_two_bars_are_refused() {
    let machine = Machine::new();
    for cfg_type in [2, 3, 4] {
        let mut caps = machine.caps();
        cap_of(&mut caps, cfg_type).bar = 0;
        assert_eq!(Layout::of(&caps), Err(Refusal::SplitAcrossBars));
    }
}

/// A capability's offset and length are the device's. One that names bytes
/// past the BAR, too few bytes, or an offset its section forbids is refused
/// before anything is read or written through it.
#[test]
fn a_structure_its_bar_does_not_hold_is_refused_and_never_reached() {
    let bar = BAR_BYTES as u32;
    let refused = |cfg_type: u8, offset: u32, length: u32| {
        let machine = Machine::new();
        let mut caps = machine.caps();
        let cap = cap_of(&mut caps, cfg_type);
        (cap.offset, cap.length) = (offset, length);
        let layout = Layout::of(&caps).expect("the capabilities are all there");
        let refusal = Offer::acknowledge(machine.clone(), &layout).err();
        if refusal.is_some() {
            assert_eq!(machine.trace(), [], "the device was reached");
        }
        refusal
    };
    for (cfg_type, name) in [(1, "COMMON_CFG"), (2, "NOTIFY_CFG"), (4, "DEVICE_CFG")] {
        assert_eq!(refused(cfg_type, bar, 0x1000), Some(Refusal::OutsideBar(name)));
        assert_eq!(refused(cfg_type, bar - 0x1000, 0x1001), Some(Refusal::OutsideBar(name)));
        // The two dwords sum past thirty-two bits, and to a small number.
        assert_eq!(refused(cfg_type, 0xFFFF_F000, 0x2000), Some(Refusal::OutsideBar(name)));
        assert_eq!(refused(cfg_type, 0x1000, u32::MAX), Some(Refusal::OutsideBar(name)));
    }
    // §4.1.4.3: through `queue_device` is 0x38 bytes.
    assert_eq!(refused(1, COMMON_AT, 0x37), Some(Refusal::TooShort("COMMON_CFG")));
    assert_eq!(refused(1, COMMON_AT, 0x38), None);
    // §4.1.4.3.1: "offset MUST be 4-byte aligned"; §4.1.4.4.1: two for the
    // notification structure's.
    assert_eq!(refused(1, COMMON_AT + 2, 0x38), Some(Refusal::Misaligned("COMMON_CFG")));
    assert_eq!(refused(2, NOTIFY_AT + 1, 0x100), Some(Refusal::Misaligned("NOTIFY_CFG")));
    // §4.1.4.6.1: "The offset for the device-specific configuration MUST be
    // 4-byte aligned", which is what a 32-bit field read there rests on.
    assert_eq!(refused(4, DEVICE_AT + 2, 6), Some(Refusal::Misaligned("DEVICE_CFG")));
    // A structure that ends on the BAR's last byte is inside it.
    assert_eq!(refused(2, bar - 0x1000, 0x1000), None);
}

// --- §3.1.1 and §2.2: the initialisation ---

/// The whole of §3.1.1 as the device sees it: every write, in order, each at
/// the width §4.1.3.1 gives its field (the stub reds on any other).
#[test]
fn the_initialisation_is_the_sequence_the_specification_orders() {
    let machine = Machine::new();
    let (_live, queue) = live(&machine);
    let features = VIRTIO_F_VERSION_1 | VIRTIO_F_ACCESS_PLATFORM | FEATURE_OFFERED;
    let (desc, avail, used) = (queue.desc_addr(), queue.avail_addr(), queue.used_addr());
    assert_eq!((desc, avail, used), (DEVICE_BASE, DEVICE_BASE + 128, DEVICE_BASE + 152));
    let common = |field, value| Event::Common { field, value };
    let registers: Vec<Event> = machine
        .trace()
        .into_iter()
        .filter(|event| matches!(event, Event::Status(_) | Event::Common { .. }))
        .collect();
    assert_eq!(
        registers,
        [
            // 1: reset. 2: ACKNOWLEDGE. 3: DRIVER.
            Event::Status(0),
            Event::Status(status::ACKNOWLEDGE),
            Event::Status(status::ACKNOWLEDGE | status::DRIVER),
            // 4: the offer read, both halves, and the subset written.
            common(0x00, 0),
            common(0x00, 1),
            common(0x08, 0),
            common(0x0C, features as u32),
            common(0x08, 1),
            common(0x0C, (features >> 32) as u32),
            // 5: FEATURES_OK (and 6, its read back, which is no write).
            Event::Status(status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK),
            // 7: the configuration vector; then the queue — selected, sized,
            // its three addresses a half at a time, its vector, and last its
            // enable (§4.1.4.3.2).
            common(0x10, ENTRY as u32),
            common(0x16, 0),
            common(0x18, SIZE as u32),
            common(0x20, desc as u32),
            common(0x24, (desc >> 32) as u32),
            common(0x28, avail as u32),
            common(0x2C, (avail >> 32) as u32),
            common(0x30, used as u32),
            common(0x34, (used >> 32) as u32),
            common(0x1A, ENTRY as u32),
            common(0x1C, 1),
            // 8: DRIVER_OK.
            Event::Status(
                status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK | status::DRIVER_OK
            ),
        ]
    );
    machine.state(|s| {
        assert_eq!(s.accepted, features);
        assert!(s.queues[0].enabled);
        assert_eq!((s.queues[0].desc, s.queues[0].driver, s.queues[0].device), (desc, avail, used));
    });
}

/// §4.1.4.3.2: the driver waits for `device_status` to read 0 "before
/// reinitializing the device". One that does not read 0 is not reinitialised.
#[test]
fn a_device_that_never_finishes_its_reset_is_refused_and_written_nothing_more() {
    let machine = Machine::new();
    machine.state(|s| {
        s.permits.reset_never_completes = true;
        s.status = status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK | status::DRIVER_OK;
    });
    let refused = Offer::acknowledge(machine.clone(), &layout(&machine)).err();
    assert_eq!(refused, Some(Refusal::ResetUnanswered));
    assert_eq!(machine.trace(), [Event::Status(0)]);
}

/// §2.2.1: "The driver MUST NOT accept a feature which the device did not
/// offer" — and the stub reds on one written. §6.1: `VERSION_1` and
/// `ACCESS_PLATFORM` are accepted where offered, whatever the caller wanted.
#[test]
fn only_what_the_device_offers_is_accepted() {
    for (offered, wanted, accepted) in [
        (
            VIRTIO_F_VERSION_1 | VIRTIO_F_ACCESS_PLATFORM | FEATURE_OFFERED,
            FEATURE_OFFERED | FEATURE_WITHHELD,
            VIRTIO_F_VERSION_1 | VIRTIO_F_ACCESS_PLATFORM | FEATURE_OFFERED,
        ),
        (
            VIRTIO_F_VERSION_1 | VIRTIO_F_ACCESS_PLATFORM | FEATURE_OFFERED,
            0,
            VIRTIO_F_VERSION_1 | VIRTIO_F_ACCESS_PLATFORM,
        ),
        (VIRTIO_F_VERSION_1 | FEATURE_OFFERED, u64::MAX, VIRTIO_F_VERSION_1 | FEATURE_OFFERED),
    ] {
        let machine = Machine::new();
        machine.state(|s| s.offered = offered);
        let offer = Offer::acknowledge(machine.clone(), &layout(&machine)).expect("acknowledged");
        assert_eq!(offer.features(), offered);
        let setup = offer.accept(wanted).expect("a subset of the offer is taken");
        assert_eq!(setup.features(), accepted);
        assert_eq!(machine.state(|s| s.accepted), accepted);
    }
}

/// §6.1: "A driver MAY fail to operate further if VIRTIO_F_VERSION_1 is not
/// offered" — this one is no legacy driver. §3.1.1: it says so with `FAILED`,
/// over the bits it had set (§2.1.1).
#[test]
fn a_device_without_version_1_is_refused_and_told_so() {
    let machine = Machine::new();
    machine.state(|s| s.offered = VIRTIO_F_ACCESS_PLATFORM | FEATURE_OFFERED);
    let refused = Offer::acknowledge(machine.clone(), &layout(&machine))
        .and_then(|offer| offer.accept(FEATURE_OFFERED))
        .err();
    assert_eq!(
        refused,
        Some(Refusal::NotVersion1 { offered: VIRTIO_F_ACCESS_PLATFORM | FEATURE_OFFERED })
    );
    assert_eq!(machine.statuses(), [0, 1, 3, 3 | status::FAILED]);
    assert_eq!(machine.state(|s| s.accepted), 0);
}

/// §3.1.1 step 6: "Re-read device status to ensure the FEATURES_OK bit is
/// still set: otherwise, the device does not support our subset of features
/// and the device is unusable."
#[test]
fn a_device_that_does_not_keep_features_ok_is_refused_and_told_so() {
    let machine = Machine::new();
    machine.state(|s| s.permits.refuses_features = true);
    let refused = Offer::acknowledge(machine.clone(), &layout(&machine))
        .and_then(|offer| offer.accept(FEATURE_OFFERED))
        .err();
    assert_eq!(
        refused,
        Some(Refusal::FeaturesRefused {
            accepted: VIRTIO_F_VERSION_1 | VIRTIO_F_ACCESS_PLATFORM | FEATURE_OFFERED,
            status: status::ACKNOWLEDGE | status::DRIVER,
        })
    );
    assert_eq!(machine.statuses(), [0, 1, 3, 11, 11 | status::FAILED]);
}

// --- §4.1.4.3 and §4.1.5.1: a queue's configuration ---

/// `queue_size` is the most the device takes, and 0 for a queue it does not
/// have (§4.1.4.3.1). Either way the queue is not enabled.
#[test]
fn a_queue_shallower_than_the_drivers_rings_is_refused() {
    let machine = Machine::new();
    let shallow = setup(&machine);
    // After the reset, which is what gives a queue its maximum back.
    machine.state(|s| s.queues[0].size = SIZE / 2);
    assert_eq!(
        shallow.enable(&queue(&machine, 0), ENTRY).err(),
        Some(Refusal::QueueTooShallow { queue: 0, offered: SIZE / 2, wanted: SIZE })
    );
    assert!(!machine.state(|s| s.queues[0].enabled));
    assert_eq!(machine.statuses().last(), Some(&(11 | status::FAILED)));

    let machine = Machine::new();
    let absent = QUEUES as u16;
    let rings = Virtqueue::new(machine.clone(), absent, SIZE, parts(0));
    assert_eq!(
        setup(&machine).enable(&rings, ENTRY).err(),
        Some(Refusal::QueueTooShallow { queue: absent, offered: 0, wanted: SIZE })
    );
}

/// §4.1.5.1.2.2: "the driver MUST verify success by reading the Vector field
/// value". A queue whose vector the device did not map is never enabled.
#[test]
fn a_vector_the_device_does_not_map_is_refused_and_its_queue_stays_disabled() {
    let machine = Machine::new();
    machine.state(|s| s.permits.no_vector_for = Some(Source::Config));
    assert_eq!(
        setup(&machine).config_vector(ENTRY).err(),
        Some(Refusal::NoVector(Source::Config))
    );
    assert_eq!(machine.statuses().last(), Some(&(11 | status::FAILED)));

    let machine = Machine::new();
    machine.state(|s| s.permits.no_vector_for = Some(Source::Queue(1)));
    let setup = setup(&machine)
        .config_vector(ENTRY)
        .and_then(|setup| setup.enable(&queue(&machine, 0), ENTRY))
        .unwrap_or_else(|why| panic!("the configuration vector and queue 0's map: {why}"));
    assert_eq!(
        setup.enable(&queue(&machine, 1), ENTRY).err(),
        Some(Refusal::NoVector(Source::Queue(1)))
    );
    machine.state(|s| assert!(s.queues[0].enabled && !s.queues[1].enabled));
    assert_eq!(machine.statuses().last(), Some(&(11 | status::FAILED)));
}

/// §4.1.4.4: the notify address is `cap.offset + queue_notify_off *
/// notify_off_multiplier`, and §4.1.4.4.1 has the structure hold the two bytes
/// written there. Both factors are the device's, so a product outside the
/// structure is refused — the stub reds on a write anywhere else.
#[test]
fn a_notify_offset_outside_the_notification_structure_is_refused() {
    let enable = |notify_off: u16, multiplier: u32| {
        let machine = Machine::new();
        machine.state(|s| s.notify_off_multiplier = multiplier);
        let setup = setup(&machine);
        // After the reset, which is what gives a queue its offset back.
        machine.state(|s| s.queues[0].notify_off = notify_off);
        let enabled = setup.enable(&queue(&machine, 0), ENTRY).map(|_| ());
        (machine, enabled)
    };
    let refused = |notify_off| Err(Refusal::Doorbell { queue: 0, notify_off });

    // 0x1000 bytes of structure: the last two-byte address in it is 0xFFE.
    for (notify_off, multiplier) in [(0x400, 4), (0xFFF, 2), (0xFFFF, u32::MAX), (1, 0x1000)] {
        let (machine, enabled) = enable(notify_off, multiplier);
        assert_eq!(enabled, refused(notify_off), "{notify_off:#x} * {multiplier:#x}");
        assert!(!machine.state(|s| s.queues[0].enabled));
    }
    // An odd address is not sixteen-bit aligned.
    assert_eq!(enable(1, 1).1, refused(1));

    for (notify_off, multiplier) in [(0x3FF, 4), (0x7FF, 2), (0xFFFF, 0)] {
        let (_machine, enabled) = enable(notify_off, multiplier);
        assert_eq!(enabled, Ok(()), "{notify_off:#x} * {multiplier:#x}");
    }
}

/// §4.1.5.2: "the driver sends an available buffer notification to the device
/// by writing the 16-bit virtqueue index of this virtqueue to the Queue Notify
/// address" — each queue's own, and one address for all where the multiplier
/// is 0 (§4.1.4.4).
#[test]
fn a_notification_is_the_queues_index_at_the_queues_address() {
    for multiplier in [NOTIFY_OFF_MULTIPLIER, 0] {
        let machine = Machine::new();
        machine.state(|s| s.notify_off_multiplier = multiplier);
        let mut queues = [queue(&machine, 0), queue(&machine, 1), queue(&machine, 2)];
        let live = queues
            .iter()
            .try_fold(setup(&machine), |setup, queue| setup.enable(queue, ENTRY))
            .unwrap_or_else(|why| panic!("the stub takes three queues: {why}"))
            .driver_ok();
        machine.forget_trace();
        for index in [2usize, 0, 1] {
            live.notify(queues[index].publish(0, &one(64, true)));
        }
        let notified: Vec<Event> =
            machine.trace().into_iter().filter(|e| matches!(e, Event::Notify { .. })).collect();
        let at = |index: usize| index * multiplier as usize;
        assert_eq!(
            notified,
            [
                Event::Notify { at: at(2), value: 2 },
                Event::Notify { at: at(0), value: 0 },
                Event::Notify { at: at(1), value: 1 },
            ]
        );
    }
}

#[test]
#[should_panic(expected = "queue 1 was published to and never enabled")]
fn a_chain_on_a_queue_the_device_was_never_given_is_the_drivers_mistake() {
    let machine = Machine::new();
    let (live, _queue) = live(&machine);
    live.notify(queue(&machine, 1).publish(0, &one(64, true)));
}

/// The device-specific structure is as long as its capability says, which is
/// the device's number: a field past it is refused and not read.
#[test]
fn a_field_past_the_device_structure_is_refused_and_not_read() {
    let machine = Machine::new();
    let mut device = setup(&machine);
    machine.forget_trace();
    let mut mac = [0u8; 6];
    for (at, byte) in mac.iter_mut().enumerate() {
        (device, *byte) =
            device.device_read8(at).unwrap_or_else(|why| panic!("byte {at} is inside: {why}"));
    }
    assert_eq!(mac, [0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    assert_eq!(machine.trace().len(), 6);
    for at in [6, 7, usize::MAX] {
        let machine = Machine::new();
        let device = setup(&machine);
        machine.forget_trace();
        assert_eq!(
            device.device_read8(at).err(),
            Some(Refusal::PastDeviceConfig { at, bytes: 6 })
        );
        assert_eq!(machine.trace(), [Event::Status(11 | status::FAILED)]);
    }
}

/// §4.1.3.1: "32-bit wide and aligned accesses for 32-bit … wide fields". A
/// field that is not a whole aligned dword of the structure is refused and not
/// read.
#[test]
fn a_32_bit_field_is_read_32_bits_wide_and_only_inside_the_structure() {
    let machine = Machine::new();
    let device = setup(&machine);
    machine.forget_trace();
    let (_, word) = device.device_read32(0).unwrap_or_else(|why| panic!("dword 0 is inside: {why}"));
    assert_eq!(word, 0x1200_5452);
    assert_eq!(machine.trace(), [Event::DeviceConfig { at: 0 }]);
    // Past the six bytes, straddling their end, off a dword, and wrapping.
    for at in [8, 4, 2, usize::MAX - 1] {
        let machine = Machine::new();
        let device = setup(&machine);
        machine.forget_trace();
        assert_eq!(device.device_read32(at).err(), Some(Refusal::PastDeviceConfig { at, bytes: 6 }));
        assert_eq!(machine.trace(), [Event::Status(11 | status::FAILED)]);
    }
}

// --- PCI 3.0 §6.7: the capability list ---

/// A function's configuration space: 256 bytes, and the offsets a claim
/// refuses.
struct Config {
    bytes: [u8; 256],
    refused: Vec<u16>,
}

impl Config {
    /// Capabilities `at`, each linked to the next and the last to none, the
    /// pointer at the first, and `Status` saying there is a list.
    fn listing(at: &[u8]) -> Self {
        let mut config = Self { bytes: [0; 256], refused: Vec::new() };
        config.bytes[0x06] = 1 << 4;
        config.bytes[0x34] = at.first().copied().unwrap_or(0);
        for (nth, &cap) in at.iter().enumerate() {
            config.bytes[cap as usize + 1] = at.get(nth + 1).copied().unwrap_or(0);
        }
        config
    }

    /// A vendor capability (§4.1.4) at `at`.
    fn vendor(&mut self, at: u8, cfg_type: u8, bar: u8, offset: u32, length: u32, mult: u32) {
        let at = at as usize;
        self.bytes[at] = 0x09;
        self.bytes[at + 3] = cfg_type;
        self.bytes[at + 4] = bar;
        self.bytes[at + 8..at + 12].copy_from_slice(&offset.to_le_bytes());
        self.bytes[at + 12..at + 16].copy_from_slice(&length.to_le_bytes());
        self.bytes[at + 16..at + 20].copy_from_slice(&mult.to_le_bytes());
    }
}

/// What a claim answers for a read it refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClaimRefused;

impl crate::pci::ConfigSpace for Config {
    type Refused = ClaimRefused;

    fn read8(&self, at: u16) -> Result<u8, ClaimRefused> {
        if self.refused.contains(&at) {
            return Err(ClaimRefused);
        }
        Ok(self.bytes[at as usize])
    }

    fn read32(&self, at: u16) -> Result<u32, ClaimRefused> {
        assert_eq!(at % 4, 0, "a 32-bit read at {at:#x}, off its alignment");
        if self.refused.contains(&at) {
            return Err(ClaimRefused);
        }
        let at = at as usize;
        Ok(u32::from_le_bytes(self.bytes[at..at + 4].try_into().unwrap()))
    }
}

/// The vendor capabilities in the list's order, any other capability passed
/// over, and the multiplier read where §4.1.4.4 puts one and nowhere else.
#[test]
fn the_walk_answers_every_vendor_capability_in_the_lists_order() {
    let mut config = Config::listing(&[0x98, 0x40, 0x70, 0x84]);
    config.vendor(0x98, 1, 4, 0x0000, 0x1000, 0);
    config.bytes[0x40] = 0x11; // MSI-X: not a vendor's.
    config.vendor(0x70, 2, 4, 0x3000, 0x1000, 4);
    config.vendor(0x84, 4, 4, 0x2000, 0x1000, 0xDEAD);
    let caps = crate::pci::vendor_caps(&config).expect("a list that ends");
    assert_eq!(
        caps,
        [
            VendorCap { cfg_type: 1, bar: 4, offset: 0, length: 0x1000, notify_off_multiplier: 0 },
            VendorCap { cfg_type: 2, bar: 4, offset: 0x3000, length: 0x1000, notify_off_multiplier: 4 },
            VendorCap { cfg_type: 4, bar: 4, offset: 0x2000, length: 0x1000, notify_off_multiplier: 0 },
        ]
    );
}

/// §6.2.3: with `Status` bit 4 clear the pointer means nothing, and nothing
/// behind it is read.
#[test]
fn a_function_without_a_capability_list_has_no_vendor_capability() {
    let mut config = Config::listing(&[0x40]);
    config.vendor(0x40, 1, 4, 0, 0x1000, 0);
    config.bytes[0x06] = 0;
    config.refused = (0x08..0x100).collect();
    assert_eq!(crate::pci::vendor_caps(&config), Ok(Vec::new()));
}

/// A read the claim refused is that refusal, at that offset, and never a
/// field of zeros another check refuses under a name of its own.
#[test]
fn a_refused_read_ends_the_walk_by_the_claims_own_word() {
    for at in [0x06, 0x34, 0x40, 0x41, 0x43, 0x44, 0x48, 0x4C, 0x50] {
        let mut config = Config::listing(&[0x40]);
        config.vendor(0x40, 2, 4, 0x3000, 0x1000, 4);
        config.refused = Vec::from([at]);
        assert_eq!(
            crate::pci::vendor_caps(&config),
            Err(WalkRefusal::Read { at, why: ClaimRefused }),
            "a refusal at {at:#x}"
        );
    }
}

/// §6.7: "the bottom two bits are Reserved and must be set to 00b. Software
/// must mask these bits off before using this register as a pointer", of the
/// pointer and of every link.
#[test]
fn every_link_is_masked_of_its_two_reserved_bits_before_it_is_an_offset() {
    let mut config = Config::listing(&[0x40, 0x60]);
    config.bytes[0x34] = 0x43;
    config.bytes[0x41] = 0x62;
    config.vendor(0x40, 1, 4, 0, 0x1000, 0);
    config.vendor(0x60, 4, 4, 0x2000, 0x1000, 0);
    let caps = crate::pci::vendor_caps(&config).expect("a list that ends");
    assert_eq!(caps.iter().map(|cap| cap.cfg_type).collect::<Vec<_>>(), [1, 4]);
}

/// A link into the header and a list that comes back round are each refused
/// by name, and neither is walked for ever.
#[test]
fn a_link_into_the_header_or_round_the_list_is_refused() {
    let mut config = Config::listing(&[0x40]);
    config.bytes[0x41] = 0x3C;
    assert_eq!(crate::pci::vendor_caps(&config), Err(WalkRefusal::IntoHeader { at: 0x3C }));

    let mut config = Config::listing(&[0x40, 0x50]);
    config.bytes[0x51] = 0x40;
    assert_eq!(crate::pci::vendor_caps(&config), Err(WalkRefusal::Looped));

    // The longest list a header holds is no loop: a capability on every dword.
    let every: Vec<u8> = (0x40..=0xFCu8).step_by(4).collect();
    assert_eq!(crate::pci::vendor_caps(&Config::listing(&every)), Ok(Vec::new()));
}

// --- §2.7: the queue ---

/// §4.1.5.1.3 step 4 has the three parts zeroed, and §2.7.10.1 has the driver
/// "initialize flags in the used ring to 0". The stub's grant starts as
/// anything but.
#[test]
fn a_new_queue_zeroes_its_parts_and_nothing_else() {
    let machine = Machine::new();
    let _queue = queue(&machine, 0);
    let parts = parts(0);
    machine.state(|s| {
        assert!(s.grant[..parts.end(SIZE)].iter().enumerate().all(|(at, byte)| {
            // The padding between the available ring and the used one is not
            // a part.
            *byte == 0 || (parts.avail + 22..parts.used).contains(&at)
        }));
        assert!(s.grant[parts.end(SIZE)..].iter().all(|byte| *byte == 0xA5));
    });
}

/// §2.7.13.1: each element's address, length and direction, chained by `next`
/// under `VIRTQ_DESC_F_NEXT` — read back the way a device reads a chain — and
/// §2.7.13.2: its head in the next available-ring entry.
#[test]
fn a_published_chain_is_what_the_device_reads() {
    let machine = Machine::new();
    let mut queue = queue(&machine, 0);
    let ring = ring(&machine, 0);
    let request = [
        Buffer::readable(BUFFERS, 16),
        Buffer::readable(BUFFERS + 0x100, 1024),
        Buffer::writable(BUFFERS + 0x800, 4),
    ];
    let _ = queue.publish(2, &request);
    let _ = queue.publish(7, &one(2048, true));
    assert_eq!(ring.avail_idx(), 2);
    assert_eq!((ring.avail_entry(0), ring.avail_entry(1)), (2, 7));
    assert_eq!(ring.chain(2), request);
    assert_eq!(ring.chain(7), one(2048, true));
}

/// §2.7.13: the descriptors and the ring entry (steps 1 and 2), a barrier
/// (4), the index (5), a barrier (6), the notification (7). A device may read
/// the chain the moment the index moves (§2.7.13.3).
#[test]
fn a_chain_is_whole_before_its_index_and_its_index_before_its_notification() {
    let machine = Machine::new();
    let (live, mut queue) = live(&machine);
    machine.forget_trace();
    live.notify(queue.publish(3, &[Buffer::readable(BUFFERS, 16), Buffer::writable(BUFFERS + 16, 64)]));

    let parts = parts(0);
    let trace = machine.trace();
    let position = |what: &dyn Fn(&Event) -> bool| {
        let found: Vec<usize> =
            trace.iter().enumerate().filter(|(_, e)| what(e)).map(|(at, _)| at).collect();
        assert!(!found.is_empty(), "missing from {trace:?}");
        found
    };
    let table = parts.desc..parts.desc + desc_bytes(SIZE);
    let descriptors = position(&|e| matches!(e, Event::Store { at, .. } if table.contains(at)));
    let entry = position(&|e| *e == Event::Store { at: parts.avail + 4, bytes: 2, value: 3 });
    let index = position(&|e| *e == Event::Store { at: parts.avail + 2, bytes: 2, value: 1 });
    let barriers = position(&|e| *e == Event::Publish);
    let notified = position(&|e| matches!(e, Event::Notify { .. }));
    // Two descriptors, four fields each.
    assert_eq!(descriptors.len(), 8);
    assert_eq!((entry.len(), index.len(), barriers.len(), notified.len()), (1, 1, 2, 1));
    assert!(descriptors.iter().all(|at| *at < barriers[0]));
    assert!(entry[0] < barriers[0]);
    assert!(barriers[0] < index[0]);
    assert!(index[0] < barriers[1]);
    assert!(barriers[1] < notified[0]);
}

/// §2.7.8.2: "The device MUST set len prior to updating the used idx" — so
/// the element is loaded after the index that counts it, across a barrier.
#[test]
fn a_used_element_is_read_after_the_index_that_counts_it() {
    let machine = Machine::new();
    let mut queue = queue(&machine, 0);
    let _ = queue.publish(0, &one(64, true));
    ring(&machine, 0).use_chain(0, 64);
    machine.forget_trace();
    assert_eq!(queue.poll_used(), Ok(Some(Used { head: 0, written: 64 })));
    let used = parts(0).used;
    assert_eq!(
        machine.trace(),
        [
            Event::Load { at: used + 2, bytes: 2 },
            Event::Observe,
            Event::Load { at: used + 4, bytes: 4 },
            Event::Load { at: used + 8, bytes: 4 },
        ]
    );
    assert_eq!(queue.poll_used(), Ok(None));
}

/// A used element's `id` is thirty-two bits and a head is sixteen: one past
/// the table is refused whole, never narrowed into it.
#[test]
fn a_head_past_the_table_is_refused() {
    let machine = Machine::new();
    let mut queue = queue(&machine, 0);
    let ring = ring(&machine, 0);
    for head in 0..4 {
        let _ = queue.publish(head, &one(64, true));
    }
    // 0x1_0002 narrows to head 2, which is in flight.
    for id in [SIZE as u32, 0x1_0002, u32::MAX] {
        ring.use_chain(id, 0);
        assert_eq!(
            queue.poll_used(),
            Err(UsedRefusal::Head(Refused::PastTable { value: id as u64, len: SIZE as u64 }))
        );
    }
    assert_eq!(queue.poll_used(), Ok(None));
    // Head 2 was not retired by the element that narrowed to it.
    ring.use_chain(2, 64);
    assert_eq!(queue.poll_used(), Ok(Some(Used { head: 2, written: 64 })));
}

/// A head nothing is in flight at: never published, or reported a second
/// time — which is how a device would hand one buffer up twice.
#[test]
fn a_head_with_no_chain_in_flight_is_refused() {
    let machine = Machine::new();
    let mut queue = queue(&machine, 0);
    let ring = ring(&machine, 0);
    let _ = queue.publish(0, &one(64, true));
    let _ = queue.publish(4, &[Buffer::readable(BUFFERS, 8), Buffer::writable(BUFFERS + 8, 8)]);
    let _ = queue.publish(6, &one(64, true));

    ring.use_chain(3, 0);
    assert_eq!(queue.poll_used(), Err(UsedRefusal::NoChain { head: 3 }));
    // Descriptor 5 is in flight and is no head.
    ring.use_chain(5, 0);
    assert_eq!(queue.poll_used(), Err(UsedRefusal::NoChain { head: 5 }));
    ring.use_chain(4, 8);
    assert_eq!(queue.poll_used(), Ok(Some(Used { head: 4, written: 8 })));
    let _ = queue.publish(1, &one(64, true));
    ring.use_chain(4, 8);
    assert_eq!(queue.poll_used(), Err(UsedRefusal::NoChain { head: 4 }));
}

/// The one that matters most: `written` becomes the length of what a driver
/// reads out of its buffer. §2.7.8: `len` is "the number of bytes written
/// into the device writable portion of the buffer", so the bound is that
/// portion and not the chain. The chain refused stays in flight, for as long
/// as the device has an element left to answer it with.
#[test]
fn more_bytes_than_the_chain_may_be_written_is_refused() {
    for len in [65, 100, 164, u32::MAX] {
        let machine = Machine::new();
        let mut queue = queue(&machine, 0);
        let ring = ring(&machine, 0);
        let _ = queue.publish(0, &[Buffer::readable(BUFFERS, 100), Buffer::writable(BUFFERS + 100, 64)]);
        let _ = queue.publish(2, &one(1514, false));
        // Two more, so the device has four elements to spend on two chains.
        let _ = queue.publish(3, &one(1, true));
        let _ = queue.publish(4, &one(1, true));

        ring.use_chain(0, len);
        assert_eq!(
            queue.poll_used(),
            Err(UsedRefusal::Written {
                head: 0,
                len: Refused::PastBound { value: len as u64, bound: 64 }
            })
        );
        ring.use_chain(0, 64);
        assert_eq!(queue.poll_used(), Ok(Some(Used { head: 0, written: 64 })));

        // A chain the device may only read has nothing to be written.
        ring.use_chain(2, 1);
        assert_eq!(
            queue.poll_used(),
            Err(UsedRefusal::Written { head: 2, len: Refused::PastBound { value: 1, bound: 0 } })
        );
        ring.use_chain(2, 0);
        assert_eq!(queue.poll_used(), Ok(Some(Used { head: 2, written: 0 })));
    }
}

/// §2.7.8: every used element answers a chain made available. A used index
/// past the available one counts elements nothing was published for, so none
/// is read — not the stale ones, and not the ring's memory under them.
#[test]
fn a_used_index_past_what_was_made_available_is_refused_and_nothing_is_read() {
    let machine = Machine::new();
    let mut queue = queue(&machine, 0);
    let ring = ring(&machine, 0);
    let _ = queue.publish(0, &one(64, true));
    let _ = queue.publish(1, &one(64, true));
    ring.write_used(0, 0, 64);
    ring.write_used(1, 1, 64);
    ring.write_used(2, 0, 64);

    for jumped in [3, 9, 0x8000, u16::MAX] {
        ring.set_used_idx(jumped);
        machine.forget_trace();
        for _ in 0..2 {
            assert_eq!(
                queue.poll_used(),
                Err(UsedRefusal::Jumped { used: jumped, taken: 0, available: 2 })
            );
        }
        let used = parts(0).used;
        assert_eq!(machine.trace(), [Event::Load { at: used + 2, bytes: 2 }; 2]);
    }

    // The index taken back to what was made available, the ring reads on.
    ring.set_used_idx(2);
    assert_eq!(queue.poll_used(), Ok(Some(Used { head: 0, written: 64 })));
    assert_eq!(queue.poll_used(), Ok(Some(Used { head: 1, written: 64 })));
    assert_eq!(queue.poll_used(), Ok(None));
    // And an index that goes backwards is one that went all the way round.
    ring.set_used_idx(1);
    assert_eq!(queue.poll_used(), Err(UsedRefusal::Jumped { used: 1, taken: 2, available: 2 }));
}

/// One element refused is one element: the ones behind it are still read.
#[test]
fn a_refused_element_does_not_hide_the_ones_behind_it() {
    let machine = Machine::new();
    let mut queue = queue(&machine, 0);
    let ring = ring(&machine, 0);
    for head in 0..3 {
        let _ = queue.publish(head, &one(64, true));
    }
    ring.use_chain(0x55, 0);
    ring.use_chain(1, 64);
    ring.use_chain(2, 3);
    assert!(matches!(queue.poll_used(), Err(UsedRefusal::Head(_))));
    assert_eq!(queue.poll_used(), Ok(Some(Used { head: 1, written: 64 })));
    assert_eq!(queue.poll_used(), Ok(Some(Used { head: 2, written: 3 })));
    assert_eq!(queue.poll_used(), Ok(None));
}

/// §2.7.5.1: "A device MUST NOT write to any descriptor table entry", and it
/// reaches the table all the same. One rewritten into a loop is a loop the
/// driver never follows: a chain is retired by what the driver recorded of
/// it, and the table is never loaded.
#[test]
fn a_descriptor_table_the_device_rewrote_into_a_loop_is_never_followed() {
    let machine = Machine::new();
    let mut queue = queue(&machine, 0);
    let ring = ring(&machine, 0);
    let chain = [Buffer::readable(BUFFERS, 16), Buffer::writable(BUFFERS + 16, 64)];
    let _ = queue.publish(0, &chain);
    let _ = queue.publish(2, &one(64, true));
    // Descriptor 0 chains to itself; descriptor 1 chains past the table.
    ring.scribble_desc(0, 1, 0);
    ring.scribble_desc(1, 1, 0xFFFF);
    ring.use_chain(0, 64);

    machine.forget_trace();
    assert_eq!(queue.poll_used(), Ok(Some(Used { head: 0, written: 64 })));
    let table = 0..desc_bytes(SIZE);
    assert!(machine
        .trace()
        .iter()
        .all(|event| !matches!(event, Event::Load { at, .. } if table.contains(at))));
    // Exactly the two descriptors of the chain came back: both take a chain
    // again, and descriptor 2 is in flight still.
    let _ = queue.publish(0, &chain);
    assert_eq!(ring.chain(0), chain);
    ring.use_chain(2, 0);
    assert_eq!(queue.poll_used(), Ok(Some(Used { head: 2, written: 0 })));
}

/// §2.7.13.3: "idx always increments, and wraps naturally at 65536" — both
/// indices, past the wrap, on a ring of eight.
#[test]
fn both_indices_wrap_at_65536() {
    let machine = Machine::new();
    let mut queue = queue(&machine, 0);
    let ring = ring(&machine, 0);
    for nth in 0..70_000u32 {
        let head = (nth % 3) as u16;
        let _ = queue.publish(head, &one(64, true));
        assert_eq!(ring.avail_entry(nth as u16), head);
        ring.use_chain(head as u32, nth % 65);
        assert_eq!(queue.poll_used(), Ok(Some(Used { head, written: nth % 65 })), "at {nth}");
        machine.forget_trace();
    }
    assert_eq!(ring.avail_idx(), (70_000u32 % 65_536) as u16);
    assert_eq!(queue.poll_used(), Ok(None));
}

/// xorshift64*: every draw comes from the seed.
struct Draws(u64);

impl Draws {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// A device that uses buffers in any order (§2.6 permits it) and, between
/// them, writes elements of its own invention. The oracle is a model of what
/// is in flight kept apart from the queue's: each element is believed exactly
/// when it names a chain in flight and no more bytes than that chain's
/// writable elements hold, and an index past the available one is refused.
///
/// Many short lives rather than one long one: every element the device invents
/// spends one a chain was owed, so a queue this device has been at for long
/// has only chains nothing can answer.
#[test]
fn a_hostile_device_is_believed_only_where_the_model_agrees() {
    let mut seeds = Draws(0x9E37_79B9_7F4A_7C15);
    let mut believed = 0u32;
    let mut refused = [0u32; 4];
    for _ in 0..2_000 {
        let seed = seeds.next() | 1;
        let mut draws = Draws(seed);
        let machine = Machine::new();
        let mut queue = queue(&machine, 0);
        let ring = ring(&machine, 0);
        // What is in flight, by head: how many descriptors, and writable bytes.
        let mut flying: BTreeMap<u16, (u16, u32)> = BTreeMap::new();
        let (mut made, mut taken) = (0u16, 0u16);

        for step in 0..64 {
            let held = |flying: &BTreeMap<u16, (u16, u32)>, at: u16| {
                flying.iter().any(|(head, (descs, _))| (*head..*head + *descs).contains(&at))
            };
            // The driver: a chain of one to three, where the table is free.
            let descs = 1 + draws.below(3) as u16;
            let head = draws.below((SIZE - descs + 1) as u64) as u16;
            if (head..head + descs).all(|at| !held(&flying, at)) {
                let readable = draws.below(descs as u64 + 1) as u16;
                let chain: Vec<Buffer> = (0..descs)
                    .map(|nth| Buffer {
                        addr: BUFFERS + nth as u64 * 0x100,
                        len: draws.below(200) as u32,
                        writable: nth >= readable,
                    })
                    .collect();
                let writable = chain.iter().filter(|b| b.writable).map(|b| b.len).sum();
                let _ = queue.publish(head, &chain);
                flying.insert(head, (descs, writable));
                made = made.wrapping_add(1);
            }

            // The device: up to three elements, each honest or invented.
            let mut written: Vec<(u32, u32)> = Vec::new();
            for _ in 0..draws.below(4) {
                let honest = flying.iter().nth(draws.below(SIZE as u64) as usize);
                let element = match (draws.below(8), honest) {
                    (0, _) => (draws.next() as u32, draws.next() as u32),
                    (1, _) => (draws.below(SIZE as u64 * 2) as u32, draws.below(400) as u32),
                    (_, Some((head, (_, writable)))) => {
                        (*head as u32, draws.below(*writable as u64 + 2) as u32)
                    }
                    (_, None) => continue,
                };
                ring.write_used(ring.used_idx().wrapping_add(written.len() as u16), element.0, element.1);
                written.push(element);
            }
            let room = made.wrapping_sub(taken);
            ring.set_used_idx(ring.used_idx().wrapping_add(written.len() as u16));
            if written.len() as u16 > room {
                let used = ring.used_idx();
                assert_eq!(
                    queue.poll_used(),
                    Err(UsedRefusal::Jumped { used, taken, available: made }),
                    "seed {seed:#x} step {step}"
                );
                refused[3] += 1;
                // The device takes back what it was never given.
                ring.set_used_idx(taken.wrapping_add(room));
                written.truncate(room as usize);
            }

            for (id, len) in written {
                let got = queue.poll_used();
                taken = taken.wrapping_add(1);
                let chain = u16::try_from(id).ok().and_then(|head| flying.get(&head).map(|c| (head, *c)));
                let want = match chain {
                    _ if id >= SIZE as u32 => Err(UsedRefusal::Head(Refused::PastTable {
                        value: id as u64,
                        len: SIZE as u64,
                    })),
                    None => Err(UsedRefusal::NoChain { head: id as u16 }),
                    Some((head, (_, writable))) if len > writable => Err(UsedRefusal::Written {
                        head,
                        len: Refused::PastBound { value: len as u64, bound: writable as u64 },
                    }),
                    Some((head, _)) => {
                        flying.remove(&head);
                        believed += 1;
                        Ok(Some(Used { head, written: len }))
                    }
                };
                match want {
                    Err(UsedRefusal::Head(_)) => refused[0] += 1,
                    Err(UsedRefusal::NoChain { .. }) => refused[1] += 1,
                    Err(UsedRefusal::Written { .. }) => refused[2] += 1,
                    Err(UsedRefusal::Jumped { .. }) | Ok(_) => {}
                }
                assert_eq!(got, want, "seed {seed:#x} step {step}: element ({id:#x}, {len:#x})");
            }
            assert_eq!(queue.poll_used(), Ok(None), "seed {seed:#x} step {step}");
            machine.forget_trace();
        }
    }
    // The runs reached every verdict, so none of them is asserted of nothing.
    assert!(believed > 5_000, "only {believed} chain(s) ever came back");
    assert!(refused.iter().all(|count| *count > 100), "refusals by kind: {refused:?}");
}

// --- the driver's own mistakes, which no device causes ---

#[test]
#[should_panic(expected = "takes descriptor 3, which is in flight")]
fn a_chain_over_a_descriptor_in_flight_is_the_drivers_mistake() {
    let machine = Machine::new();
    let mut queue = queue(&machine, 0);
    let _ = queue.publish(2, &[Buffer::readable(BUFFERS, 8), Buffer::writable(BUFFERS + 8, 8)]);
    let _ = queue.publish(3, &one(64, true));
}

/// §2.7.4.2: "The driver MUST place any device-writable descriptor elements
/// after any device-readable descriptor elements."
#[test]
#[should_panic(expected = "a device-readable element after a device-writable one")]
fn a_readable_element_after_a_writable_one_is_the_drivers_mistake() {
    let machine = Machine::new();
    let mut queue = queue(&machine, 0);
    let _ = queue.publish(0, &[Buffer::writable(BUFFERS, 8), Buffer::readable(BUFFERS + 8, 8)]);
}

#[test]
#[should_panic(expected = "runs past a table of 8")]
fn a_chain_past_the_table_is_the_drivers_mistake() {
    let machine = Machine::new();
    let mut queue = queue(&machine, 0);
    let _ = queue.publish(7, &[Buffer::readable(BUFFERS, 8), Buffer::writable(BUFFERS + 8, 8)]);
}

/// §4.1.4.3.2: "the driver MUST NOT write a value which is not a power of 2
/// to queue_size".
#[test]
#[should_panic(expected = "6 descriptors is no queue size")]
fn a_queue_size_that_is_no_power_of_two_is_the_drivers_mistake() {
    let machine = Machine::new();
    let _ = Virtqueue::new(machine, 0, 6, Parts::contiguous(0, 6));
}

#[test]
#[should_panic(expected = "runs past a grant of 0x8000 bytes")]
fn a_ring_past_the_grant_is_the_drivers_mistake() {
    let machine = Machine::new();
    let _ = Virtqueue::new(machine, 0, SIZE, Parts::contiguous(GRANT_BYTES - 0x80, SIZE));
}

/// §2.7.1: each part on its alignment. A used ring two bytes off a multiple
/// of four is one the device may not take.
#[test]
#[should_panic(expected = "its used ring at 0x9a is not on a multiple of 4")]
fn a_part_off_its_alignment_is_the_drivers_mistake() {
    let machine = Machine::new();
    let _ = Virtqueue::new(machine, 0, SIZE, Parts { desc: 0, avail: 128, used: 154 });
}
