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
    /// it.
    ///
    /// A claim that refuses the read for anything but `WouldBlock` is the
    /// kernel saying this function is no longer this process's: a fault at the
    /// unit is the one that happens, and by the time it is answered the
    /// function's bus mastering is gone. Every frame from here on is one that
    /// silently never arrives, so this dies where it can be read — once, for
    /// whichever driver is running.
    ///
    /// So does an Intel part that kept the frames a link change left in its
    /// transmit ring past the driver's deadline: nothing drives it from there.
    ///
    /// Answers the link where the pass found it changed; virtio reports none.
    pub fn begin_pass(&self) -> Option<toyos_i219::Link> {
        let answered = match self {
            Self::Virtio(nic) => nic.take_interrupt().map(|_| None).map_err(toyos_i219::PassRefused::Claim),
            Self::Intel(nic) => nic.begin_pass(),
        };
        answered.unwrap_or_else(|why| panic!("netstack: this NIC cannot be driven on — {why}"))
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
        snap.put("descriptors.stranded", counters.stranded);
        snap.put("transmit.full", counters.tx_full);
        snap.put("transmit.wake_armed", counters.tx_wake_armed);
        snap.put("transmit.wake_taken", counters.tx_wake_taken);
        snap.put("wire.sent", wire.sent);
        snap.put("wire.received", wire.received);
        snap.put("wire.seen", wire.seen);
        snap.put("errors.missed", wire.missed);
        snap.put("errors.crc", wire.crc_errors);
    }

    /// Hands the next received frame to `take` and gives its buffer back to
    /// the card once `take` returns; `false` when none waits.
    pub fn rx(&self, take: impl FnOnce(&[u8])) -> bool {
        use crate::prof::{Slot, add, now, since};
        let t0 = now();
        match self {
            Self::Virtio(nic) => {
                let polled = nic.poll_rx();
                since(Slot::CyPollRx, t0);
                let Some((index, len)) = polled else { return false };
                take(nic.rx_frame(index, len));
                let t0 = now();
                nic.rx_done(index);
                since(Slot::CyRxDone, t0);
            }
            Self::Intel(nic) => {
                let polled = nic.poll_rx();
                since(Slot::CyPollRx, t0);
                let Some(frame) = polled else { return false };
                take(nic.rx_frame(&frame));
                let t0 = now();
                nic.rx_done(frame);
                since(Slot::CyRxDone, t0);
            }
        }
        add(Slot::RxDone, 1);
        true
    }

    /// MEASUREMENT ONLY: the card's `RDT` writes so far; virtio has none.
    pub fn rdt_writes(&self) -> u64 {
        match self {
            Self::Virtio(_) => 0,
            Self::Intel(nic) => u64::from(nic.rdt_writes()),
        }
    }

    /// How many frames the card takes now. [`Self::tx`] is for a caller this
    /// answered: a card with no room is not offered a frame.
    pub fn tx_room(&self) -> usize {
        match self {
            Self::Virtio(nic) => nic.tx_room(),
            Self::Intel(nic) => nic.tx_room(),
        }
    }

    /// Have the claim read ready when room returns, and answer the room there
    /// is once that is so: a caller answered 0 has nothing to do before the
    /// claim wakes it.
    ///
    /// virtio is asked nothing: its every finished transmit interrupts
    /// (`VirtioNet::tx_room`).
    pub fn wake_on_room(&self) -> usize {
        match self {
            Self::Virtio(nic) => nic.tx_room(),
            Self::Intel(nic) => nic.wake_on_room(),
        }
    }

    pub fn tx<R>(&self, len: usize, fill: impl FnOnce(&mut [u8]) -> R) -> R {
        match self {
            Self::Virtio(nic) => nic.tx(len, fill),
            Self::Intel(nic) => nic.tx(len, fill),
        }
    }

    /// How long until a pass has to begin whether or not the claim reads
    /// ready, in nanoseconds: [`Self::begin_pass`] is where a card that owes
    /// something by then is refused. virtio owes nothing.
    pub fn pass_due_in(&self) -> Option<u64> {
        match self {
            Self::Virtio(_) => None,
            Self::Intel(nic) => nic.pass_due_in(),
        }
    }

    /// Say what the driver counted, once a pass and after every frame the pass
    /// sent: a line per refused descriptor is itself more frames to send.
    /// virtio counts nothing: what its device is not believed on ends it.
    pub fn report(&self) {
        match self {
            Self::Virtio(_) => {}
            Self::Intel(nic) => nic.report(),
        }
    }
}
