//! The device half of the mix loop, and the two devices behind it: the
//! virtio-sound function soundserver drives itself, and an output driver's
//! ring (`crate::output`).
//!
//! Everything that differs between them is a method below. The
//! mixer, the ramps, the DLL, the underrun accounting and the suspend/resume
//! structure are one body of code either way — and [`Pipeline`] exists to keep
//! that honest, because the two differ in *who owns a period soundserver has not
//! refilled* and nothing else in the loop can be written without knowing which.

use toyos::shm::Bytes;
use toyos::AsHandle;

use crate::virtio;

/// Who owns a period soundserver has been given back and has not refilled.
///
/// The mix loop's free list was written for [`Pipeline::Queue`] throughout, and
/// three of its rules are that model showing: it holds a period back while a
/// client is mid-refill, it drains by not submitting, and it takes the lowest
/// free index first because the device plays what it is given in the order it
/// is given.
///
/// [`Pipeline::Ring`] breaks all three, which is what killed soundserver on the T14.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pipeline {
    /// virtio-sound: a period soundserver has not submitted is a period the device
    /// does not have. Holding one costs nothing, indefinitely, and the play
    /// order is the submit order.
    Queue,
    /// An output driver's (HDA's) ring: the engine owns every period for as long as it runs. It returns to
    /// buffer `i` exactly `num_buffers` periods after completing it and plays
    /// whatever is there, so a period soundserver holds back is played as the
    /// silence `released` left in it *and* completed a second time — a
    /// completion for a buffer soundserver still holds. The play order is the
    /// ring's, which is the lowest free index only while a batch does not wrap.
    Ring,
}

/// Periods the device has given back, and when it said so.
#[derive(Clone, Copy)]
pub(crate) struct Completion {
    pub(crate) mask: u32,
    /// When the device's first word about these periods landed: where a
    /// wake's lateness is split between the device and soundserver.
    pub(crate) first_nanos: u64,
    /// When its newest landed: the time of the last of the first `clocked`
    /// periods, in play order.
    pub(crate) last_nanos: u64,
    /// How many of these periods `last_nanos` answers for. The rest completed
    /// after the message it is the time of, and a later record clocks them.
    pub(crate) clocked: u32,
}

/// The device half of the mix loop.
///
/// Two implementations and no more: what differs between the two devices is
/// exactly the methods below; the mixer, the ramps, the DLL, the underrun
/// accounting and the suspend/resume structure are one body of code either way.
pub(crate) trait Backend {
    /// Who owns a freed period soundserver does not refill.
    fn pipeline(&self) -> Pipeline;

    /// The handle a completion arrives on.
    fn handle(&self) -> toyos::BorrowedHandle<'_>;

    /// Period `idx` of the memory the device plays from, mapped once at
    /// claim, which the driver or the device may reach meanwhile.
    fn period(&self, idx: usize) -> Bytes<'_>;

    /// The periods given back since the last call, or `None` for none.
    fn completion(&mut self) -> Option<Completion>;

    /// Period `idx` has played and is soundserver's again.
    ///
    /// HDA's engine is cyclic and never stops on its own, so a period nobody
    /// refills is *replayed* — audible harm. Zeroing it as it frees makes a late
    /// soundserver cost silence instead, which is exactly what virtio-sound's device
    /// does when it runs dry. virtio's own implementation is empty for that
    /// reason and not by omission: nothing is published for a period
    /// that was not filled, so the device is never given it again.
    fn released(&mut self, idx: usize);

    /// Period `idx` holds `bytes` of PCM and is the device's to play.
    ///
    /// On a [`Pipeline::Ring`] that is a statement about the *contents*: the
    /// period was already the device's and this only says it now holds audio.
    fn submit(&mut self, idx: usize, bytes: usize);

    /// Stop the stream. Idempotent, and cheap when it is already stopped.
    fn stop(&mut self);

    /// Why the device is no longer played to, once it is not: an output
    /// driver that hung up or broke its protocol, which ends the device loop
    /// and never soundserver.
    fn ended(&self) -> Option<&str> {
        None
    }
}

pub(crate) struct VirtioBackend {
    pub(crate) virtio: virtio::Virtio,
}

impl Backend for VirtioBackend {
    fn pipeline(&self) -> Pipeline {
        Pipeline::Queue
    }

    fn handle(&self) -> toyos::BorrowedHandle<'_> {
        self.virtio.dev().as_handle()
    }

    fn period(&self, idx: usize) -> Bytes<'_> {
        self.virtio.period(idx)
    }

    fn completion(&mut self) -> Option<Completion> {
        self.virtio.completion()
    }

    fn released(&mut self, _idx: usize) {}

    fn submit(&mut self, idx: usize, bytes: usize) {
        self.virtio.submit(idx, bytes);
    }

    fn stop(&mut self) {
        self.virtio.stop();
    }
}
