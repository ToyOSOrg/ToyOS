//! The compositor's virtio-gpu driver: one scanout through the device's two
//! queues, its page flip and its cursor, every decision about them, and none
//! of the instructions that carry them out.
//!
//! Every `§` here is a section of *Virtual I/O Device (VIRTIO) Version 1.2*,
//! OASIS Committee Specification 01. [`wire`] is §5.7.6's messages; this
//! module is the queues they travel on, laid out in one grant, over
//! `toyos-virtio`'s split virtqueue and the [`Claim`] its holder implements.
//!
//! # Every control command is fenced
//!
//! §5.7.6.5 lets the device return a command before processing it unless the
//! command carries `VIRTIO_GPU_FLAG_FENCE`. So every control command carries
//! it and an id of its own, and an answer without the flag or with another id
//! is refused: no completion this driver relies on is assumed. Commands whose
//! order matters are not in flight together — a frame's transfers are
//! answered before its flushes are sent — since §5.7.6.5 promises completion
//! and not order.
//!
//! # The device is not trusted
//!
//! Every word it writes back — a used element, an answer, a configuration
//! field — is input from outside the driver. What `toyos-virtio` bounds it
//! bounds; what is the GPU device's is bounded here, and one that fails is a
//! [`Refusal`] by name, the end of the device's use. One answer is not a
//! fault: `RESP_ERR_OUT_OF_MEMORY` to the three commands that make a scanout
//! is the device declining a mode ([`Declined`]), and the mode on screen
//! stays.
//!
//! # Events
//!
//! One interrupt serves both queues and configuration changes. Every wake of
//! the claim, inside a command's wait too, drains the cursor queue and reads
//! `events_read`, so a display event a wait consumed is noted
//! ([`Gpu::display_changed`]) and never lost. [`Gpu::output`] clears the
//! events before it asks for the output, so one raised during its reads
//! raises another (§5.7.4.2).
//!
//! # What is not here
//!
//! No 3D, no blob resources, one scanout (scanout 0), and one backing entry
//! per resource: a lent scanout is one contiguous run of device addresses.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod wire;

#[cfg(test)]
mod tests;

use toyos_device_memory::DmaBuffers;
use toyos_virtio::pci;
use toyos_virtio::queue::{Buffer, Parts, Published, Used, UsedRefusal, Virtqueue};

use wire::{Command, Rect, CURSOR_SIDE, FLAG_FENCE, HDR_BYTES};

/// Descriptors per queue, powers of two (§2.7). Every chain is two: a
/// request the device reads and an answer it writes.
pub const CONTROL_QUEUE_SIZE: u16 = 16;
pub const CURSOR_QUEUE_SIZE: u16 = 16;
const CHAIN: u16 = 2;
const CONTROL_SLOTS: usize = (CONTROL_QUEUE_SIZE / CHAIN) as usize;
const CURSOR_SLOTS: usize = (CURSOR_QUEUE_SIZE / CHAIN) as usize;

/// The longest request: `RESOURCE_ATTACH_BACKING` with its one entry.
const REQUEST_BYTES: u32 = HDR_BYTES + 8 + 16;
/// §5.7.6.10's `virtio_gpu_update_cursor`.
const CURSOR_REQUEST_BYTES: u32 = HDR_BYTES + 32;

/// The grant's layout: each queue on a page of its own, then each control
/// slot's request and answer, each cursor slot's, and the cursor's image.
const CONTROL_PARTS: Parts = Parts::contiguous(0x0000, CONTROL_QUEUE_SIZE);
const CURSOR_PARTS: Parts = Parts::contiguous(0x1000, CURSOR_QUEUE_SIZE);
const CONTROL_SLOT_AT: usize = 0x2000;
const CONTROL_SLOT_BYTES: usize = 0x500;
const CONTROL_ANSWER: usize = 0x40;
const CURSOR_SLOT_AT: usize = CONTROL_SLOT_AT + CONTROL_SLOTS * CONTROL_SLOT_BYTES;
const CURSOR_SLOT_BYTES: usize = 0x80;
const CURSOR_ANSWER: usize = 0x40;
const CURSOR_IMAGE: usize = 0x5000;
const CURSOR_IMAGE_BYTES: u32 = CURSOR_SIDE * CURSOR_SIDE * 4;
pub const GRANT_BYTES: usize = CURSOR_IMAGE + CURSOR_IMAGE_BYTES as usize;

const _: () = {
    assert!(CONTROL_PARTS.end(CONTROL_QUEUE_SIZE) <= CURSOR_PARTS.desc);
    assert!(CURSOR_PARTS.end(CURSOR_QUEUE_SIZE) <= CONTROL_SLOT_AT);
    assert!(REQUEST_BYTES as usize <= CONTROL_ANSWER);
    assert!(CONTROL_ANSWER + wire::EDID_BYTES as usize <= CONTROL_SLOT_BYTES);
    assert!(CURSOR_REQUEST_BYTES as usize <= CURSOR_ANSWER);
    assert!(CURSOR_ANSWER + HDR_BYTES as usize <= CURSOR_SLOT_BYTES);
    assert!(CURSOR_SLOT_AT + CURSOR_SLOTS * CURSOR_SLOT_BYTES <= CURSOR_IMAGE);
};

/// The cursor's resource; every scanout's counts up from the next.
const CURSOR_RESOURCE: u32 = 1;
const FIRST_SCANOUT_RESOURCE: u32 = 2;

/// The one scanout this driver drives.
const SCANOUT: u32 = 0;

/// What the driver needs of the claim it runs on.
pub trait Claim {
    /// A region lent to the function, which the holder draws a scanout into.
    type Backing;
    /// Why the holder could not lend one.
    type Refused: Copy + core::fmt::Debug;

    /// Tell the device of the chain `published` made available.
    fn notify(&self, published: Published);
    /// §5.7.4's `events_read` and `events_clear`.
    fn events_read(&mut self) -> Result<u32, pci::Refusal>;
    fn events_clear(&mut self, events: u32) -> Result<(), pci::Refusal>;
    /// Block until the claim's interrupt record is readable, and take it;
    /// `false` when the holder's bound passed first.
    fn wait(&mut self) -> bool;
    /// `bytes` of memory the function may read, contiguous at the device
    /// address answered beside it.
    fn lend(&mut self, bytes: u64) -> Result<(Self::Backing, u64), Self::Refused>;
    /// Take back what [`Self::lend`] answered: the device reaches none of it
    /// from here.
    fn unlend(&mut self, backing: Self::Backing);
}

/// Why the device is not driven. Each is something it answered, or did not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A used ring `toyos-virtio` did not believe.
    Used(u16, UsedRefusal),
    /// It never answered: the holder's wait passed its bound.
    Silent(Command),
    /// It answered with fewer bytes than the answer's structure.
    ShortAnswer { command: Command, written: u32, wanted: u32 },
    /// The answer to a fenced command does not carry the fence.
    Unfenced { command: Command, flags: u32, fence: u64, sent: u64 },
    /// It answered with an error, or with another command's answer.
    Answered { command: Command, answer: u32 },
    /// A cursor command's answer is neither none nor a header (§5.7.6.10).
    CursorAnswer { command: Command, written: u32 },
    /// Its configuration, as the transport refused it.
    Config(pci::Refusal),
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Used(queue, why) => write!(f, "its queue {queue}'s used ring: {why}"),
            Self::Silent(command) => write!(f, "it never answered {}", command.name()),
            Self::ShortAnswer { command, written, wanted } => write!(
                f,
                "it answered {} with {written} byte(s), where the answer is {wanted}",
                command.name()
            ),
            Self::Unfenced { command, flags, fence, sent } => write!(
                f,
                "it answered {} with flags {flags:#x} and fence {fence}, where it was sent fence \
                 {sent}",
                command.name()
            ),
            Self::Answered { command, answer } => match wire::answer_name(*answer) {
                Some(name) => write!(f, "it answered {} with {name}", command.name()),
                None => write!(f, "it answered {} with {answer:#x}, which is no answer", command.name()),
            },
            Self::CursorAnswer { command, written } => write!(
                f,
                "it answered {} with {written} byte(s), which is neither none nor a header",
                command.name()
            ),
            Self::Config(why) => write!(f, "its configuration: {why}"),
        }
    }
}

impl From<pci::Refusal> for Refusal {
    fn from(why: pci::Refusal) -> Self {
        Self::Config(why)
    }
}

/// Why a mode was not taken. The mode on screen stays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Declined<E> {
    /// The holder could not lend a scanout.
    Lend(E),
    /// The device answered `RESP_ERR_OUT_OF_MEMORY` to this command.
    Device(Command),
}

/// A mode's step: done, or declined with the mode on screen kept.
pub type Taken<T, E> = Result<T, Declined<E>>;

/// What the device says of its output (§5.7.6.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Output {
    /// Scanout 0's preferred size, `None` where it is disabled.
    pub preferred: Option<(u32, u32)>,
    /// Its EDID's base block, where `VIRTIO_GPU_F_EDID` was negotiated.
    pub edid: Option<[u8; 128]>,
}

/// A scanout resource: created, backed, and the region behind it.
struct Scanout<B> {
    id: u32,
    mode: (u32, u32),
    backing: B,
}

/// A scanout made and not yet shown: [`Gpu::prepare`] answers it, the holder
/// draws its first frame into [`Self::backing`], and [`Gpu::show`] takes it.
pub struct Next<B>(Scanout<B>);

impl<B> Next<B> {
    pub fn backing(&self) -> &B {
        &self.0.backing
    }

    pub fn mode(&self) -> (u32, u32) {
        self.0.mode
    }
}

/// A control command in flight, at its slot.
#[derive(Clone, Copy)]
struct InFlight {
    command: Command,
    fence: u64,
}

/// What an answered control command came to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Done,
    OutOfMemory,
}

/// The two queues, laid out in the grant before the transport is given them:
/// [`toyos_virtio::pci::Setup::enable`] takes each, and [`Gpu::open`] takes
/// them back once the device is live.
pub struct Queues<M: DmaBuffers> {
    pub control: Virtqueue<M>,
    pub cursor: Virtqueue<M>,
}

impl<M: DmaBuffers + Clone> Queues<M> {
    /// # Panics
    /// If the grant is shorter than [`GRANT_BYTES`]: the caller's own mistake.
    pub fn new(mem: M) -> Self {
        Self {
            control: Virtqueue::new(mem.clone(), wire::CONTROL_QUEUE, CONTROL_QUEUE_SIZE, CONTROL_PARTS),
            cursor: Virtqueue::new(mem, wire::CURSOR_QUEUE, CURSOR_QUEUE_SIZE, CURSOR_PARTS),
        }
    }
}

/// The device, live.
pub struct Gpu<M: DmaBuffers, C: Claim> {
    mem: M,
    claim: C,
    queues: Queues<M>,
    control: [Option<InFlight>; CONTROL_SLOTS],
    cursor: [Option<Command>; CURSOR_SLOTS],
    next_fence: u64,
    next_resource: u32,
    edid: bool,
    display_changed: bool,
    scanout: Option<Scanout<C::Backing>>,
    /// The hot spot of the image last written to the cursor's resource, once
    /// that resource is made.
    cursor_hot: Option<(u32, u32)>,
}

impl<M: DmaBuffers, C: Claim> Gpu<M, C> {
    /// The device past `DRIVER_OK`, with `queues` enabled on it. `edid` is
    /// whether `VIRTIO_GPU_F_EDID` was negotiated.
    pub fn open(queues: Queues<M>, mem: M, claim: C, edid: bool) -> Self {
        Self {
            mem,
            claim,
            queues,
            control: [None; CONTROL_SLOTS],
            cursor: [None; CURSOR_SLOTS],
            next_fence: 1,
            next_resource: FIRST_SCANOUT_RESOURCE,
            edid,
            display_changed: false,
            scanout: None,
            cursor_hot: None,
        }
    }

    pub fn claim(&self) -> &C {
        &self.claim
    }

    /// The mode on screen, once one is.
    pub fn mode(&self) -> Option<(u32, u32)> {
        self.scanout.as_ref().map(|s| s.mode)
    }

    /// The region the scanout on screen is drawn into.
    pub fn backing(&self) -> Option<&C::Backing> {
        self.scanout.as_ref().map(|s| &s.backing)
    }

    /// What a wake of the claim owes, its record already taken: the cursor
    /// queue drained, and `events_read` read for a display event.
    pub fn wake(&mut self) -> Result<(), Refusal> {
        self.drain_cursor()?;
        if self.claim.events_read()? & wire::EVENT_DISPLAY != 0 {
            self.display_changed = true;
        }
        Ok(())
    }

    /// Whether a wake found a display event since the last [`Self::output`].
    pub fn display_changed(&self) -> bool {
        self.display_changed
    }

    /// The output as the device describes it now (§5.7.5): the events
    /// cleared, then `GET_DISPLAY_INFO`, then `GET_EDID` for scanout 0.
    pub fn output(&mut self) -> Result<Output, Refusal> {
        let events = self.claim.events_read()?;
        self.claim.events_clear(events)?;
        self.display_changed = false;

        let slot = self.command(Command::GetDisplayInfo, &[])?.0;
        // §5.7.6.8: `pmodes[0]` follows the header: its rect, `enabled`, `flags`.
        let at = Self::answer_at(slot) + HDR_BYTES as usize;
        let (width, height) = (self.mem.read32(at + 8), self.mem.read32(at + 12));
        let enabled = self.mem.read32(at + 16) != 0;
        let preferred = enabled.then_some((width, height));

        let edid = if self.edid {
            let slot = self.command(Command::GetEdid, &[SCANOUT, 0])?.0;
            let at = Self::answer_at(slot) + wire::EDID_AT;
            let mut block = [0u8; 128];
            for (nth, word) in block.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                *word = self.mem.read32(at + 4 * nth).to_le_bytes();
            }
            Some(block)
        } else {
            None
        };
        Ok(Output { preferred, edid })
    }

    /// Make a scanout of `mode` (§5.7.6.1): lend its backing, create its
    /// resource, attach the backing. The mode on screen stays until
    /// [`Self::show`].
    ///
    /// # Panics
    /// If `mode` is empty or its scanout is 4 GiB or more: the caller bounds
    /// the modes it asks for.
    pub fn prepare(&mut self, mode: (u32, u32)) -> Result<Taken<Next<C::Backing>, C::Refused>, Refusal> {
        let (width, height) = mode;
        let bytes = u64::from(width) * u64::from(height) * 4;
        let length = u32::try_from(bytes)
            .ok()
            .filter(|b| *b > 0)
            .unwrap_or_else(|| panic!("virtio-gpu: {width}x{height} is no mode this driver makes"));
        let (backing, addr) = match self.claim.lend(bytes) {
            Ok(lent) => lent,
            Err(why) => return Ok(Err(Declined::Lend(why))),
        };
        let id = self.next_resource;
        self.next_resource = id.checked_add(1).expect("virtio-gpu: resource ids run out after 2^32 modes");

        let create = [id, wire::FORMAT_B8G8R8X8_UNORM, width, height];
        if self.command(Command::ResourceCreate2d, &create)?.1 == Outcome::OutOfMemory {
            self.claim.unlend(backing);
            return Ok(Err(Declined::Device(Command::ResourceCreate2d)));
        }
        let attach = [id, 1, addr as u32, (addr >> 32) as u32, length, 0];
        if self.command(Command::ResourceAttachBacking, &attach)?.1 == Outcome::OutOfMemory {
            self.unref(id)?;
            self.claim.unlend(backing);
            return Ok(Err(Declined::Device(Command::ResourceAttachBacking)));
        }
        Ok(Ok(Next(Scanout { id, mode, backing })))
    }

    /// Put `next` on screen (§5.7.6.3): its whole frame transferred,
    /// `SET_SCANOUT`, a whole flush; then the scanout it replaced unref'd and,
    /// only once that is answered, its backing given back.
    pub fn show(&mut self, next: Next<C::Backing>) -> Result<Taken<(), C::Refused>, Refusal> {
        let Next(new) = next;
        let whole = Rect::whole(new.mode.0, new.mode.1);
        self.transfer(new.id, whole, new.mode.0)?;
        self.settle()?;
        let mut set = [0; 6];
        set[..4].copy_from_slice(&whole.words());
        set[4..].copy_from_slice(&[SCANOUT, new.id]);
        if self.command(Command::SetScanout, &set)?.1 == Outcome::OutOfMemory {
            self.unref(new.id)?;
            self.claim.unlend(new.backing);
            return Ok(Err(Declined::Device(Command::SetScanout)));
        }
        self.flush(new.id, whole)?;
        self.settle()?;
        if let Some(old) = self.scanout.replace(new) {
            self.unref(old.id)?;
            self.claim.unlend(old.backing);
        }
        Ok(Ok(()))
    }

    /// Carry `rects` of the scanout on screen to the display (§5.7.6.2):
    /// every transfer answered, then every flush.
    ///
    /// # Panics
    /// With no scanout on screen, or a rectangle outside it: the caller's
    /// own mistake.
    pub fn present(&mut self, rects: &[Rect]) -> Result<(), Refusal> {
        let (id, (width, height)) = {
            let s = self.scanout.as_ref().expect("virtio-gpu: a present with no scanout on screen");
            (s.id, s.mode)
        };
        for r in rects {
            assert!(
                r.x.checked_add(r.width).is_some_and(|x| x <= width)
                    && r.y.checked_add(r.height).is_some_and(|y| y <= height),
                "virtio-gpu: {r:?} is outside the {width}x{height} scanout"
            );
        }
        for r in rects {
            self.transfer(id, *r, width)?;
        }
        self.settle()?;
        for r in rects {
            self.flush(id, *r)?;
        }
        self.settle()
    }

    /// The cursor's image and hotspot, at `at` (§5.7.6.6): its resource made
    /// the first time, the image transferred and answered, then
    /// `UPDATE_CURSOR`. `image` is 64x64 pixels whose bytes are blue, green,
    /// red and alpha.
    pub fn cursor(&mut self, image: &[u32; (CURSOR_SIDE * CURSOR_SIDE) as usize], hot: (u32, u32), at: (u32, u32)) -> Result<(), Refusal> {
        if self.cursor_hot.is_none() {
            let create = [CURSOR_RESOURCE, wire::FORMAT_B8G8R8A8_UNORM, CURSOR_SIDE, CURSOR_SIDE];
            self.required(Command::ResourceCreate2d, &create)?;
            let addr = self.mem.device_addr(CURSOR_IMAGE);
            let attach = [CURSOR_RESOURCE, 1, addr as u32, (addr >> 32) as u32, CURSOR_IMAGE_BYTES, 0];
            self.required(Command::ResourceAttachBacking, &attach)?;
        }
        self.cursor_hot = Some(hot);
        for (nth, pixel) in image.iter().enumerate() {
            self.mem.write32(CURSOR_IMAGE + 4 * nth, *pixel);
        }
        self.transfer(CURSOR_RESOURCE, Rect::whole(CURSOR_SIDE, CURSOR_SIDE), CURSOR_SIDE)?;
        self.settle()?;
        self.cursor_send(Command::UpdateCursor, at, CURSOR_RESOURCE, hot)
    }

    /// Move the cursor to `at` (§5.7.6.10). The request carries the cursor's
    /// resource and hot spot, which §5.7.6.10 has the device ignore: QEMU 11.1
    /// hides the cursor on a `MOVE_CURSOR` whose resource is 0.
    pub fn move_cursor(&mut self, at: (u32, u32)) -> Result<(), Refusal> {
        match self.cursor_hot {
            Some(hot) => self.cursor_send(Command::MoveCursor, at, CURSOR_RESOURCE, hot),
            None => self.cursor_send(Command::MoveCursor, at, 0, (0, 0)),
        }
    }

    fn answer_at(slot: usize) -> usize {
        CONTROL_SLOT_AT + slot * CONTROL_SLOT_BYTES + CONTROL_ANSWER
    }

    fn transfer(&mut self, id: u32, r: Rect, width: u32) -> Result<(), Refusal> {
        // §5.7.6.8: the offset into the resource's backing of the rect's first
        // byte, which this driver lays out row after row of `width` pixels.
        let offset = (u64::from(r.y) * u64::from(width) + u64::from(r.x)) * 4;
        let mut words = [0; 8];
        words[..4].copy_from_slice(&r.words());
        words[4..].copy_from_slice(&[offset as u32, (offset >> 32) as u32, id, 0]);
        self.send(Command::TransferToHost2d, &words).map(drop)
    }

    fn flush(&mut self, id: u32, r: Rect) -> Result<(), Refusal> {
        let mut words = [0; 6];
        words[..4].copy_from_slice(&r.words());
        words[4..].copy_from_slice(&[id, 0]);
        self.send(Command::ResourceFlush, &words).map(drop)
    }

    fn unref(&mut self, id: u32) -> Result<(), Refusal> {
        self.required(Command::ResourceUnref, &[id, 0])
    }

    /// A command no answer but success may end.
    fn required(&mut self, command: Command, payload: &[u32]) -> Result<(), Refusal> {
        match self.command(command, payload)?.1 {
            Outcome::Done => Ok(()),
            Outcome::OutOfMemory => {
                Err(Refusal::Answered { command, answer: wire::RESP_ERR_OUT_OF_MEMORY })
            }
        }
    }

    /// One command alone in flight, sent and answered: its slot, where its
    /// answer is, and what it came to.
    fn command(&mut self, command: Command, payload: &[u32]) -> Result<(usize, Outcome), Refusal> {
        assert!(
            self.control.iter().all(Option::is_none),
            "virtio-gpu: {} sent with commands still in flight",
            command.name()
        );
        self.send(command, payload)?;
        let (slot, _, outcome) = self.reap()?;
        Ok((slot, outcome))
    }

    /// Publish `command` on a free control slot, fenced, waiting for one
    /// where every slot is in flight.
    fn send(&mut self, command: Command, payload: &[u32]) -> Result<usize, Refusal> {
        let slot = loop {
            match self.control.iter().position(Option::is_none) {
                Some(free) => break free,
                None => {
                    self.reap()?;
                }
            }
        };
        let fence = self.next_fence;
        self.next_fence += 1;
        let at = CONTROL_SLOT_AT + slot * CONTROL_SLOT_BYTES;
        let words = wire::header(command, FLAG_FENCE, fence).into_iter().chain(payload.iter().copied());
        let mut bytes = 0;
        for (nth, word) in words.enumerate() {
            self.mem.write32(at + 4 * nth, word);
            bytes += 4;
        }
        let chain = [
            Buffer::readable(self.mem.device_addr(at), bytes),
            Buffer::writable(self.mem.device_addr(at + CONTROL_ANSWER), command.ok().1),
        ];
        let published = self.queues.control.publish(slot as u16 * CHAIN, &chain);
        self.claim.notify(published);
        self.control[slot] = Some(InFlight { command, fence });
        Ok(slot)
    }

    /// Every control command in flight answered, each answer success.
    fn settle(&mut self) -> Result<(), Refusal> {
        while self.control.iter().any(Option::is_some) {
            if let (_, command, Outcome::OutOfMemory) = self.reap()? {
                return Err(Refusal::Answered { command, answer: wire::RESP_ERR_OUT_OF_MEMORY });
            }
        }
        Ok(())
    }

    /// The next control command the device answers, waiting for it, and its
    /// answer held to the command: the fence, the length and the type.
    fn reap(&mut self) -> Result<(usize, Command, Outcome), Refusal> {
        let used = loop {
            if let Some(used) = used(&mut self.queues.control)? {
                break used;
            }
            let waiting = self.control.iter().flatten().next().expect("virtio-gpu: a wait with nothing in flight").command;
            if !self.claim.wait() {
                return Err(Refusal::Silent(waiting));
            }
            self.wake()?;
        };
        let Used { head, written } = used;
        // A head `toyos-virtio` answered is one a chain was published at, and
        // this driver publishes one only at a slot's.
        let slot = (head / CHAIN) as usize;
        let InFlight { command, fence: sent } =
            self.control[slot].take().expect("virtio-gpu: toyos-virtio answered a slot with nothing in flight");
        if written < HDR_BYTES {
            return Err(Refusal::ShortAnswer { command, written, wanted: HDR_BYTES });
        }
        let at = Self::answer_at(slot);
        let answer = self.mem.read32(at);
        let flags = self.mem.read32(at + 4);
        let fence = u64::from(self.mem.read32(at + 8)) | u64::from(self.mem.read32(at + 12)) << 32;
        if flags & FLAG_FENCE == 0 || fence != sent {
            return Err(Refusal::Unfenced { command, flags, fence, sent });
        }
        let (ok, wanted) = command.ok();
        if answer == ok {
            if written < wanted {
                return Err(Refusal::ShortAnswer { command, written, wanted });
            }
            return Ok((slot, command, Outcome::Done));
        }
        let makes_a_scanout = matches!(
            command,
            Command::ResourceCreate2d | Command::ResourceAttachBacking | Command::SetScanout
        );
        if answer == wire::RESP_ERR_OUT_OF_MEMORY && makes_a_scanout {
            return Ok((slot, command, Outcome::OutOfMemory));
        }
        Err(Refusal::Answered { command, answer })
    }

    /// Publish a cursor command, unfenced: §5.7.6.10 has the cursor queue
    /// carry no data the driver waits on.
    fn cursor_send(&mut self, command: Command, at: (u32, u32), resource: u32, hot: (u32, u32)) -> Result<(), Refusal> {
        let slot = loop {
            self.drain_cursor()?;
            match self.cursor.iter().position(Option::is_none) {
                Some(free) => break free,
                None if self.claim.wait() => self.wake()?,
                None => return Err(Refusal::Silent(command)),
            }
        };
        let base = CURSOR_SLOT_AT + slot * CURSOR_SLOT_BYTES;
        let payload = [SCANOUT, at.0, at.1, 0, resource, hot.0, hot.1, 0];
        for (nth, word) in wire::header(command, 0, 0).into_iter().chain(payload).enumerate() {
            self.mem.write32(base + 4 * nth, word);
        }
        let chain = [
            Buffer::readable(self.mem.device_addr(base), CURSOR_REQUEST_BYTES),
            Buffer::writable(self.mem.device_addr(base + CURSOR_ANSWER), HDR_BYTES),
        ];
        let published = self.queues.cursor.publish(slot as u16 * CHAIN, &chain);
        self.claim.notify(published);
        self.cursor[slot] = Some(command);
        Ok(())
    }

    /// Every cursor command the device has finished with. A used `len` of 0
    /// is no answer; one of a header is one, held to the control queue's rule.
    fn drain_cursor(&mut self) -> Result<(), Refusal> {
        while let Some(Used { head, written }) = used(&mut self.queues.cursor)? {
            let slot = (head / CHAIN) as usize;
            let command = self.cursor[slot].take().expect("virtio-gpu: toyos-virtio answered a cursor slot with nothing in flight");
            match written {
                0 => {}
                HDR_BYTES => {
                    let answer = self.mem.read32(CURSOR_SLOT_AT + slot * CURSOR_SLOT_BYTES + CURSOR_ANSWER);
                    if answer != wire::RESP_OK_NODATA {
                        return Err(Refusal::Answered { command, answer });
                    }
                }
                written => return Err(Refusal::CursorAnswer { command, written }),
            }
        }
        Ok(())
    }
}

/// The next chain `queue`'s device has finished with, or the used ring's
/// refusal under the queue's own index.
fn used<M: DmaBuffers>(queue: &mut Virtqueue<M>) -> Result<Option<Used>, Refusal> {
    let index = queue.index();
    queue.poll_used().map_err(|why| Refusal::Used(index, why))
}
