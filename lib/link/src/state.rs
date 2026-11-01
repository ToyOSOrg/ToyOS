//! What the driver's state words say, and their encodings.

/// The link as its driver reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Down,
    Up { mbps: u32, full_duplex: bool },
    /// The device reports no link at all, as virtio's network device without
    /// `VIRTIO_NET_F_STATUS` does; the stack takes it as up.
    Unreported,
    /// The device reports its link up and no speed, as virtio's network
    /// device with `VIRTIO_NET_F_STATUS` and without `VIRTIO_NET_F_SPEED_DUPLEX`.
    UpWithoutSpeed,
}

const DOWN: u32 = 0;
const UNREPORTED: u32 = 1;
const UP: u32 = 2;
const UP_WITHOUT_SPEED: u32 = 3;
const FULL_DUPLEX: u32 = 1 << 2;
const MBPS_SHIFT: u32 = 8;

impl Link {
    pub fn is_up(self) -> bool {
        !matches!(self, Self::Down)
    }

    /// Its word. A speed past what eight bits short of a word holds is
    /// written as the most that does.
    pub fn encode(self) -> u32 {
        match self {
            Self::Down => DOWN,
            Self::Unreported => UNREPORTED,
            Self::UpWithoutSpeed => UP_WITHOUT_SPEED,
            Self::Up { mbps, full_duplex } => {
                let mbps = mbps.clamp(1, u32::MAX >> MBPS_SHIFT);
                UP | if full_duplex { FULL_DUPLEX } else { 0 } | mbps << MBPS_SHIFT
            }
        }
    }

    /// The link a driver's word names, or `None` for a word no driver writes.
    pub fn decode(word: u32) -> Option<Self> {
        let reserved = word & 0xf8;
        let mbps = word >> MBPS_SHIFT;
        match word & 0b11 {
            DOWN if word == DOWN => Some(Self::Down),
            UNREPORTED if word == UNREPORTED => Some(Self::Unreported),
            UP_WITHOUT_SPEED if word == UP_WITHOUT_SPEED => Some(Self::UpWithoutSpeed),
            UP if reserved == 0 && mbps > 0 => Some(Self::Up { mbps, full_duplex: word & FULL_DUPLEX != 0 }),
            _ => None,
        }
    }
}

/// A count a driver may report, under the name `inspect` reads it by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Counter {
    DescriptorsSent,
    DescriptorsReceived,
    DescriptorsStranded,
    TransmitFull,
    TransmitWakeArmed,
    TransmitWakeTaken,
    TransmitFullNanos,
    TransmitUnreported,
    TransmitUnreportedMax,
    WireSent,
    WireReceived,
    WireSeen,
    ErrorsMissed,
    ErrorsCrc,
}

impl Counter {
    pub const ALL: [Self; 14] = [
        Self::DescriptorsSent,
        Self::DescriptorsReceived,
        Self::DescriptorsStranded,
        Self::TransmitFull,
        Self::TransmitWakeArmed,
        Self::TransmitWakeTaken,
        Self::TransmitFullNanos,
        Self::TransmitUnreported,
        Self::TransmitUnreportedMax,
        Self::WireSent,
        Self::WireReceived,
        Self::WireSeen,
        Self::ErrorsMissed,
        Self::ErrorsCrc,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::DescriptorsSent => "descriptors.sent",
            Self::DescriptorsReceived => "descriptors.received",
            Self::DescriptorsStranded => "descriptors.stranded",
            Self::TransmitFull => "transmit.full",
            Self::TransmitWakeArmed => "transmit.wake_armed",
            Self::TransmitWakeTaken => "transmit.wake_taken",
            Self::TransmitFullNanos => "transmit.full.ns",
            Self::TransmitUnreported => "transmit.unreported",
            Self::TransmitUnreportedMax => "transmit.unreported.max",
            Self::WireSent => "wire.sent",
            Self::WireReceived => "wire.received",
            Self::WireSeen => "wire.seen",
            Self::ErrorsMissed => "errors.missed",
            Self::ErrorsCrc => "errors.crc",
        }
    }

    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

/// What a driver counted, each counter it reports with its value. The values
/// are the driver's word: read for display, never for a decision.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    values: [Option<u64>; Counter::ALL.len()],
}

impl Counters {
    pub fn set(&mut self, counter: Counter, value: u64) {
        self.values[counter.index()] = Some(value);
    }

    pub fn get(&self, counter: Counter) -> Option<u64> {
        self.values[counter.index()]
    }

    /// Every counter reported, in [`Counter::ALL`]'s order.
    pub fn iter(&self) -> impl Iterator<Item = (Counter, u64)> + '_ {
        Counter::ALL.into_iter().filter_map(|c| self.get(c).map(|v| (c, v)))
    }

    /// Which counters are reported, a bit each.
    pub(crate) fn present(&self) -> u32 {
        Counter::ALL.into_iter().filter(|c| self.get(*c).is_some()).fold(0, |mask, c| mask | 1 << c.index())
    }
}

/// A multicast group's address, as the stack joins it and the driver passes
/// frames sent to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Group([u8; 6]);

impl Group {
    /// `None` for an address that is no group's: its first byte's low bit,
    /// IEEE 802's individual/group bit, clear.
    pub fn new(mac: [u8; 6]) -> Option<Self> {
        (mac[0] & 1 == 1).then_some(Self(mac))
    }

    pub fn mac(self) -> [u8; 6] {
        self.0
    }

    pub(crate) fn encode(self) -> [u32; 2] {
        let m = self.0;
        [u32::from_le_bytes([m[0], m[1], m[2], m[3]]), u32::from_le_bytes([m[4], m[5], 0, 0])]
    }

    pub(crate) fn decode(words: [u32; 2]) -> Option<Self> {
        let [a, b, c, d] = words[0].to_le_bytes();
        let [e, f, rest @ ..] = words[1].to_le_bytes();
        if rest != [0, 0] {
            return None;
        }
        Self::new([a, b, c, d, e, f])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_link_reads_back_as_written() {
        for link in [
            Link::Down,
            Link::Unreported,
            Link::UpWithoutSpeed,
            Link::Up { mbps: 10, full_duplex: false },
            Link::Up { mbps: 1000, full_duplex: true },
            Link::Up { mbps: u32::MAX >> 8, full_duplex: true },
        ] {
            assert_eq!(Link::decode(link.encode()), Some(link));
        }
        assert_eq!(Link::decode(Link::Up { mbps: u32::MAX, full_duplex: false }.encode()), Some(Link::Up { mbps: u32::MAX >> 8, full_duplex: false }));
    }

    /// Every word no driver writes is refused: an up link of no speed, a
    /// reserved bit, a down, unreported or speedless link carrying fields.
    #[test]
    fn a_word_no_driver_writes_names_no_link() {
        for word in [UP, UP | FULL_DUPLEX, UP | 1 << 3 | 1 << 8, DOWN | 1 << 8, UNREPORTED | FULL_DUPLEX, UP_WITHOUT_SPEED | 1 << 8, UP_WITHOUT_SPEED | FULL_DUPLEX, u32::MAX] {
            assert_eq!(Link::decode(word), None, "{word:#x}");
        }
    }

    #[test]
    fn a_group_is_a_group_address() {
        let mdns = [0x01, 0x00, 0x5e, 0x00, 0x00, 0xfb];
        let group = Group::new(mdns).unwrap();
        assert_eq!(Group::decode(group.encode()), Some(group));
        assert_eq!(Group::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]), None, "an individual address");
        let [lo, hi] = group.encode();
        assert_eq!(Group::decode([lo, hi | 1 << 16]), None, "bytes past the address");
    }

    #[test]
    fn counters_report_only_what_was_set() {
        let mut counters = Counters::default();
        assert_eq!(counters.present(), 0);
        counters.set(Counter::ErrorsCrc, 3);
        counters.set(Counter::DescriptorsSent, u64::MAX);
        assert_eq!(counters.iter().collect::<Vec<_>>(), [(Counter::DescriptorsSent, u64::MAX), (Counter::ErrorsCrc, 3)]);
        assert_eq!(counters.present(), 1 | 1 << 13);
        let names: std::collections::BTreeSet<_> = Counter::ALL.iter().map(|c| c.name()).collect();
        assert_eq!(names.len(), Counter::ALL.len());
        assert!(Counter::ALL.iter().enumerate().all(|(i, c)| c.index() == i));
    }
}
