use toyos_i219::Part;
use toyos_inspect::Snapshot;

use crate::i219;
use crate::virtio_net::VirtioNet;

/// The NIC this program drives, whichever one the manifest gave it.
///
/// **One enum and not a trait object**: there are two of them, both known at
/// build time, and what a `dyn` would buy is a vtable on the frame path.
pub enum Card {
    Virtio(VirtioNet),
    Intel(i219::Nic),
}

impl Card {
    /// A device this driver cannot bring up is not a machine without a NIC: the
    /// claim was minted, so something the device said is not what this driver
    /// understands, and that is loud.
    fn undrivable(why: impl std::fmt::Display) -> ! {
        panic!("netstack: the NIC this program was given is not one it can drive — {why}")
    }

    pub fn intel(claim: toyos::PciDev, part: Part) -> Self {
        match i219::Nic::open(claim, part) {
            Ok(nic) => Self::Intel(nic),
            Err(why) => Self::undrivable(why),
        }
    }

    pub fn virtio(claim: toyos::PciDev) -> Self {
        match VirtioNet::open(claim) {
            Ok(nic) => Self::Virtio(nic),
            Err(why) => Self::undrivable(why),
        }
    }

    pub fn mac(&self) -> [u8; 6] {
        match self {
            Self::Virtio(nic) => nic.mac(),
            Self::Intel(nic) => nic.mac(),
        }
    }

    /// The claim, for the poller: readable means an interrupt has landed.
    pub fn claim(&self) -> &toyos::PciDev {
        match self {
            Self::Virtio(nic) => nic.claim(),
            Self::Intel(nic) => nic.claim(),
        }
    }

    /// **The record has to be taken, not merely noticed.** A claim reads ready
    /// while it holds an undrained interrupt, so a pass that saw the token and
    /// left it would find the same one on the next `wait` and every one after
    /// it. What the message meant is in the rings, which `iface.poll` reads.
    /// This is also where a driver with a per-pass budget gets it back.
    ///
    /// A claim that refuses the read for anything but `WouldBlock` is the
    /// kernel saying this function is no longer this process's: a fault at the
    /// unit is the one that happens, and by the time it is answered the
    /// function's bus mastering is gone. Every frame from here on is one that
    /// silently never arrives, so this dies where it can be read — once, for
    /// whichever driver is running.
    ///
    /// Answers the link where the pass found it changed; virtio reports none.
    pub fn begin_pass(&self) -> Option<toyos_i219::Link> {
        let answered = match self {
            Self::Virtio(nic) => nic.take_interrupt().map(|_| None),
            Self::Intel(nic) => nic.begin_pass(),
        };
        answered
            .unwrap_or_else(|why| panic!("netstack: this NIC's claim refused an interrupt read: {why:?}"))
    }

    /// What `inspect` reads about the card: which driver, its address, its
    /// link, and on the Intel parts what the driver and the MAC counted.
    ///
    /// **virtio's link is `unreported`, not `up`**: the device tells netstack
    /// nothing about one, and netstack serving as though it were up is netstack's
    /// assumption rather than something it measured.
    pub fn inspect(&self, snap: &mut Snapshot) {
        let m = self.mac();
        snap.put(
            "mac",
            format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5]),
        );
        let nic = match self {
            Self::Virtio(_) => {
                snap.put("driver", "virtio-net");
                snap.put("link.state", "unreported");
                return;
            }
            Self::Intel(nic) => nic,
        };
        snap.put(
            "driver",
            match nic.part() {
                Part::I219 => "i219",
                Part::E82574 => "82574",
            },
        );
        match nic.link() {
            toyos_i219::Link::Down => snap.put("link.state", "down"),
            toyos_i219::Link::Up { speed, full_duplex } => {
                snap.put("link.state", "up");
                snap.put(
                    "link.speed_mbps",
                    match speed {
                        toyos_i219::Speed::Mbps10 => 10u32,
                        toyos_i219::Speed::Mbps100 => 100,
                        toyos_i219::Speed::Mbps1000 => 1000,
                    },
                );
                snap.put("link.duplex", if full_duplex { "full" } else { "half" });
            }
        }
        let (counters, wire) = nic.counts();
        snap.put("descriptors.sent", counters.sent);
        snap.put("descriptors.received", counters.received);
        snap.put("wire.sent", wire.sent);
        snap.put("wire.received", wire.received);
        snap.put("wire.seen", wire.seen);
        snap.put("errors.missed", wire.missed);
        snap.put("errors.crc", wire.crc_errors);
    }

    pub fn tx<R>(&self, len: usize, fill: impl FnOnce(&mut [u8]) -> R) -> R {
        match self {
            Self::Virtio(nic) => nic.tx(len, fill),
            Self::Intel(nic) => nic.tx(len, fill),
        }
    }

    /// Say what the driver counted, once a pass and after every frame the pass
    /// sent: a line per dropped frame is itself more frames to send.
    pub fn report(&self) {
        match self {
            Self::Virtio(nic) => nic.report(),
            Self::Intel(nic) => nic.report(),
        }
    }
}
