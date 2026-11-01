//! Framed IPC over connections.
//!
//! A frame is `[u32 msg_type][payload]` and the handles it carries, sent as
//! one kernel message (`SYS_MESSAGE_SEND`) and taken whole
//! (`SYS_MESSAGE_RECV`): a receiver never sees part of a frame, and a
//! frame's handles arrive with it and with no other.

use toyos_abi::RawHandle;
use toyos_abi::syscall::{self, Received, SyscallError};
use crate::{AsHandle, BorrowedHandle, OwnedHandle};

/// A type that may cross an IPC boundary as a payload.
///
/// # Safety
/// Implementors must be `#[repr(C)]`, contain no padding bytes, hold no
/// pointers, and have no invalid bit patterns. All three are load-bearing:
/// [`send`] publishes `size_of::<T>()` bytes of the sender's memory to a peer,
/// so a padding byte no field owns is whatever the sender's stack held; and
/// [`decode_payload`] reinterprets bytes a *peer* chose, so a `bool`, a
/// fieldless enum or a `NonZeroU32` would be UB the moment it arrives.
///
/// Declare payload structs with [`ipc_payload!`], which proves the first two
/// mechanically. A hand-written `unsafe impl` is checking them by eye.
///
/// `usize` and `isize` are deliberately absent: a wire type has one width.
pub unsafe trait IpcPayload: Copy {
    /// Exists so [`ipc_payload!`]'s padding assertion can only sum field
    /// types that are payloads themselves — which is what makes the
    /// no-padding property hold through a nested struct.
    #[doc(hidden)]
    const SIZE: usize = core::mem::size_of::<Self>();
}

macro_rules! payload_primitives {
    ($($t:ty),* $(,)?) => { $(unsafe impl IpcPayload for $t {})* };
}
payload_primitives!(u8, u16, u32, u64, i8, i16, i32, i64, f32, f64);

unsafe impl<T: IpcPayload, const N: usize> IpcPayload for [T; N] {}

/// Declare a `#[repr(C)]` IPC payload struct.
///
/// The expansion asserts that the struct's size equals the sum of its field
/// sizes, which for `repr(C)` holds exactly when there is no padding, and it
/// sums them through [`IpcPayload::SIZE`], so every field must be a payload
/// too. A struct that would publish bytes no field owns does not compile, and
/// neither does one carrying a type whose bit patterns are not all valid.
#[macro_export]
macro_rules! ipc_payload {
    ($(
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $($(#[$fmeta:meta])* $fvis:vis $field:ident : $ty:ty),* $(,)?
        }
    )+) => {$(
        $(#[$meta])*
        #[repr(C)]
        #[derive(Clone, Copy)]
        $vis struct $name {
            $($(#[$fmeta])* $fvis $field: $ty,)*
        }

        const _: () = assert!(
            core::mem::size_of::<$name>()
                == 0 $(+ <$ty as $crate::ipc::IpcPayload>::SIZE)*,
            concat!(
                stringify!($name),
                " has padding: an IPC payload must not publish bytes no field owns",
            ),
        );

        unsafe impl $crate::ipc::IpcPayload for $name {}
    )+};
}

/// The largest payload this endpoint will frame, in either direction.
///
/// Derived, not picked: the largest payload struct in the tree is
/// `window::CreateWindowRequest` at 40 bytes, and the largest byte payload any
/// protocol here produces is 4096 — the threshold at which `window`'s
/// clipboard set and the compositor's paste both switch to shared memory
/// instead. 8192 is one doubling of headroom, so a protocol can grow its
/// largest inline message without the SDK becoming the thing that refuses it.
/// It sizes the buffer every send frames in, and the receive buffer of every
/// protocol that takes frames up to it.
pub const MAX_FRAME_LEN: u32 = 8192;

/// The message type's bytes in front of every payload.
const TYPE_LEN: usize = 4;

const _: () = assert!(TYPE_LEN + MAX_FRAME_LEN as usize <= syscall::MAX_MESSAGE_LEN);

/// A frame as a send lays it out for the kernel: the message type, then
/// its payload. Bytes only, so no padding.
#[repr(C)]
struct Wire<P> {
    msg_type: [u8; TYPE_LEN],
    payload: P,
}

#[derive(Debug)]
pub enum IpcError {
    Disconnected,
    /// The peer's frame is not one this endpoint can read: larger than the
    /// receiver holds, too short for a message type, or another payload than
    /// the caller asked for.
    Malformed,
    /// This endpoint will not frame a payload that large. Distinct from
    /// [`IpcError::Malformed`] because the two blame different sides.
    TooLarge,
    Syscall(SyscallError),
}

/// Why a whole frame could not be handed over without blocking.
#[derive(Debug)]
pub enum TrySendError {
    /// The peer has no room for the whole frame now. **Nothing was sent**: a
    /// frame goes in whole or not at all. A peer that cannot take one frame
    /// has a queue of unread messages behind it, and is dropped.
    Full,
    /// This endpoint will not frame a payload that large.
    TooLarge,
    Syscall(SyscallError),
}

/// An IPC connection. A server gets one from [`crate::port::Acceptor::accept`],
/// a client from [`crate::endow::service`], and a server one end a peer
/// handed it from [`Self::received`].
pub struct Connection(pub(crate) OwnedHandle);

impl AsHandle for Connection {
    fn as_handle(&self) -> BorrowedHandle<'_> { self.0.as_handle() }
}

impl Connection {
    /// A connection end a peer sent, consumed. Its type is the sender's
    /// word: the kernel refuses each connection call on a handle of another
    /// type, so trust the sender only as far as its protocol says.
    pub fn received(handle: OwnedHandle) -> Self {
        Self(handle)
    }

    /// This end as a handle, to send it on in a frame ([`Self::send_with`]).
    pub fn into_owned(self) -> OwnedHandle {
        self.0
    }

    /// Give up ownership, for an end about to be endowed.
    pub fn into_raw(self) -> RawHandle {
        self.0.into_raw()
    }

    pub fn send<T: IpcPayload>(&self, msg_type: u32, payload: &T) -> Result<(), IpcError> {
        send(self.as_handle(), msg_type, payload)
    }

    pub fn signal(&self, msg_type: u32) -> Result<(), IpcError> {
        signal(self.as_handle(), msg_type)
    }

    pub fn send_bytes(&self, msg_type: u32, data: &[u8]) -> Result<(), IpcError> {
        send_bytes(self.as_handle(), msg_type, data)
    }

    pub fn try_send<T: IpcPayload>(&self, msg_type: u32, payload: &T) -> Result<(), TrySendError> {
        try_send(self.as_handle(), msg_type, payload)
    }

    pub fn try_signal(&self, msg_type: u32) -> Result<(), TrySendError> {
        try_signal(self.as_handle(), msg_type)
    }

    pub fn try_send_bytes(&self, msg_type: u32, data: &[u8]) -> Result<(), TrySendError> {
        try_send_bytes(self.as_handle(), msg_type, data)
    }

    /// A frame and the handles it carries, as one message.
    ///
    /// **Every handle is consumed, sent or not**: a refused send closes them
    /// all, so no caller holds a handle it believes it gave away. More than
    /// [`MAX_TRANSFER_HANDLES`](syscall::MAX_TRANSFER_HANDLES) is refused
    /// `InvalidArgument`, as the kernel refuses it.
    pub fn send_with<T: IpcPayload>(
        &self,
        msg_type: u32,
        payload: &T,
        handles: impl IntoIterator<Item = OwnedHandle>,
    ) -> Result<(), IpcError> {
        message(self.as_handle(), msg_type, as_bytes(payload), handles, false).map_err(blocking)
    }

    /// [`send_with`](Self::send_with) for a byte payload.
    pub fn send_bytes_with(
        &self,
        msg_type: u32,
        data: &[u8],
        handles: impl IntoIterator<Item = OwnedHandle>,
    ) -> Result<(), IpcError> {
        message(self.as_handle(), msg_type, data, handles, false).map_err(blocking)
    }

    /// [`send_with`](Self::send_with) for a bare message type.
    pub fn signal_with(&self, msg_type: u32, handles: impl IntoIterator<Item = OwnedHandle>) -> Result<(), IpcError> {
        message(self.as_handle(), msg_type, &[], handles, false).map_err(blocking)
    }

    /// [`send_with`](Self::send_with) without blocking: nothing is sent and
    /// every handle is closed when the frame does not fit now.
    pub fn try_send_with<T: IpcPayload>(
        &self,
        msg_type: u32,
        payload: &T,
        handles: impl IntoIterator<Item = OwnedHandle>,
    ) -> Result<(), TrySendError> {
        message(self.as_handle(), msg_type, as_bytes(payload), handles, true).map_err(nonblocking)
    }

    /// [`try_send_with`](Self::try_send_with) for a byte payload.
    pub fn try_send_bytes_with(
        &self,
        msg_type: u32,
        data: &[u8],
        handles: impl IntoIterator<Item = OwnedHandle>,
    ) -> Result<(), TrySendError> {
        message(self.as_handle(), msg_type, data, handles, true).map_err(nonblocking)
    }

    /// [`try_send_with`](Self::try_send_with) for a bare message type.
    pub fn try_signal_with(
        &self,
        msg_type: u32,
        handles: impl IntoIterator<Item = OwnedHandle>,
    ) -> Result<(), TrySendError> {
        message(self.as_handle(), msg_type, &[], handles, true).map_err(nonblocking)
    }

    /// Wait for the next frame and take its payload straight into
    /// `payload`, with the handles it carried. For a caller that keeps the
    /// payload in its own buffer: a [`FrameRx`] would be one more copy. A
    /// payload longer than `payload` is [`IpcError::Malformed`] and stays
    /// queued; otherwise it fails as [`FrameRx::recv`] does.
    pub fn recv_into(&self, payload: &mut [u8]) -> Result<(Frame, Batch), IpcError> {
        let mut msg_type = [0; TYPE_LEN];
        receive(self.as_handle(), &mut msg_type, payload, false).map_err(Refused::blocking)
    }
}

// Free functions, for a connection a caller holds by something other than a
// `Connection`.

pub fn send<T: IpcPayload>(conn: BorrowedHandle<'_>, msg_type: u32, payload: &T) -> Result<(), IpcError> {
    message(conn, msg_type, as_bytes(payload), none(), false).map_err(blocking)
}

pub fn signal(conn: BorrowedHandle<'_>, msg_type: u32) -> Result<(), IpcError> {
    message(conn, msg_type, &[], none(), false).map_err(blocking)
}

pub fn send_bytes(conn: BorrowedHandle<'_>, msg_type: u32, data: &[u8]) -> Result<(), IpcError> {
    message(conn, msg_type, data, none(), false).map_err(blocking)
}

/// [`Connection::signal_with`] on a raw handle.
pub(crate) fn signal_with(conn: BorrowedHandle<'_>, msg_type: u32, handles: impl IntoIterator<Item = OwnedHandle>) -> Result<(), IpcError> {
    message(conn, msg_type, &[], handles, false).map_err(blocking)
}

/// Send a framed payload without ever blocking, or refuse the whole frame.
///
/// A server writing to a client it does not trust to read cannot use [`send`]:
/// a blocking send parks in the kernel until the peer takes its messages,
/// which is a client deciding when the server runs again. This is the same
/// frame, and the peer's backlog is an answer rather than a wait.
pub fn try_send<T: IpcPayload>(conn: BorrowedHandle<'_>, msg_type: u32, payload: &T) -> Result<(), TrySendError> {
    message(conn, msg_type, as_bytes(payload), none(), true).map_err(nonblocking)
}

/// [`signal`] without blocking.
pub fn try_signal(conn: BorrowedHandle<'_>, msg_type: u32) -> Result<(), TrySendError> {
    message(conn, msg_type, &[], none(), true).map_err(nonblocking)
}

/// [`send_bytes`] without blocking.
pub fn try_send_bytes(conn: BorrowedHandle<'_>, msg_type: u32, data: &[u8]) -> Result<(), TrySendError> {
    message(conn, msg_type, data, none(), true).map_err(nonblocking)
}

fn none() -> core::iter::Empty<OwnedHandle> {
    core::iter::empty()
}

/// Why [`message`] sent nothing.
enum Unsent {
    TooLarge,
    Kernel(SyscallError),
}

fn blocking(e: Unsent) -> IpcError {
    match e {
        Unsent::TooLarge => IpcError::TooLarge,
        Unsent::Kernel(e) => IpcError::Syscall(e),
    }
}

fn nonblocking(e: Unsent) -> TrySendError {
    match e {
        Unsent::TooLarge => TrySendError::TooLarge,
        Unsent::Kernel(SyscallError::WouldBlock) => TrySendError::Full,
        Unsent::Kernel(e) => TrySendError::Syscall(e),
    }
}

/// `msg_type` and `payload` laid out in `frame`, answering the bytes the
/// kernel sends, or `None` for a payload past [`MAX_FRAME_LEN`]. Only those
/// bytes are written: nothing else of the buffer is read.
fn framed<'a>(
    frame: &'a mut core::mem::MaybeUninit<Wire<[u8; MAX_FRAME_LEN as usize]>>,
    msg_type: u32,
    payload: &[u8],
) -> Option<&'a [u8]> {
    if payload.len() > MAX_FRAME_LEN as usize {
        return None;
    }
    let base = frame.as_mut_ptr() as *mut u8;
    // SAFETY: the buffer holds `TYPE_LEN + MAX_FRAME_LEN` bytes, the payload
    // fits behind the type, and the slice covers only the bytes written here.
    unsafe {
        core::ptr::copy_nonoverlapping(msg_type.to_ne_bytes().as_ptr(), base, TYPE_LEN);
        core::ptr::copy_nonoverlapping(payload.as_ptr(), base.add(TYPE_LEN), payload.len());
        Some(core::slice::from_raw_parts(base, TYPE_LEN + payload.len()))
    }
}

/// The one way a frame leaves this endpoint: its type and `payload` in one
/// buffer, and `handles`, handed to the kernel as one message. Every handle is
/// consumed whatever the answer.
fn message<H: AsHandle>(
    conn: BorrowedHandle<'_>,
    msg_type: u32,
    payload: &[u8],
    handles: impl IntoIterator<Item = H>,
    nonblock: bool,
) -> Result<(), Unsent> {
    let mut frame = core::mem::MaybeUninit::uninit();
    let Some(bytes) = framed(&mut frame, msg_type, payload) else {
        // Consumed all the same.
        handles.into_iter().for_each(drop);
        return Err(Unsent::TooLarge);
    };
    move_batch(handles, |batch| {
        // SAFETY: `move_batch` hands over only handles it was given to
        // consume, and forgets them once they are sent.
        unsafe { syscall::message_send(conn.raw(), bytes, batch, nonblock) }
    })
    .map_err(Unsent::Kernel)
}

/// Hand `handles` to `send` as one batch, consuming every one: what `send`
/// takes is the peer's, and what it refuses is closed here as it drops.
fn move_batch<H: AsHandle>(
    handles: impl IntoIterator<Item = H>,
    send: impl FnOnce(&[RawHandle]) -> Result<(), SyscallError>,
) -> Result<(), SyscallError> {
    let mut batch: [Option<H>; syscall::MAX_TRANSFER_HANDLES] = core::array::from_fn(|_| None);
    let mut count = 0;
    for handle in handles {
        *batch.get_mut(count).ok_or(SyscallError::InvalidArgument)? = Some(handle);
        count += 1;
    }
    let mut raw = [toyos_abi::HANDLE_INVALID; syscall::MAX_TRANSFER_HANDLES];
    for (out, handle) in raw.iter_mut().zip(batch.iter().flatten()) {
        *out = handle.as_handle().raw();
    }
    send(&raw[..count])?;
    #[expect(clippy::disallowed_methods, reason = "taken: the numbers are no longer this process's to close")]
    batch.into_iter().flatten().for_each(core::mem::forget);
    Ok(())
}

/// One payload as a `T`, for a reader holding a frame's bytes
/// ([`FrameRx::payload`]): exactly one `T`, or [`IpcError::Malformed`].
pub fn decode_payload<T: IpcPayload>(bytes: &[u8]) -> Result<T, IpcError> {
    if bytes.len() != core::mem::size_of::<T>() {
        return Err(IpcError::Malformed);
    }
    // SAFETY: the source is exactly one `T` long, and `IpcPayload` promises
    // every bit pattern is a valid `T`.
    Ok(unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const T) })
}

/// What one [`FrameRx::pump`] found.
#[derive(Clone, Copy, Debug)]
pub enum RxStep {
    /// The connection has nothing more right now.
    Idle,
    /// A whole frame was taken; its payload is [`FrameRx::payload`] and its
    /// `handles` are [`FrameRx::take_handles`]'s.
    Frame { msg_type: u32, payload_len: usize, handles: usize },
    /// The peer hung up, or its handle is gone.
    Eof,
    /// A frame this receiver cannot take: too short for a message type, or
    /// larger than its `MAX` or carrying handles this process's table has no
    /// room for, either of which stays queued in front of every later frame.
    /// The peer is dropped.
    Malformed,
}

/// A frame [`FrameRx::recv`] took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub msg_type: u32,
    pub payload_len: usize,
    pub handles: usize,
}

/// One peer's inbound frames: a buffer for the widest payload the caller's
/// protocol carries, `MAX`, and the handles the last frame brought.
///
/// **A server never reads a client with a blocking receive** — that hands a
/// client the decision of when the server runs again — so it [`pump`]s,
/// which never blocks. A client waiting on its server's answer [`recv`]s.
/// A frame wider than `MAX` is refused whole and never cut short. A protocol
/// whose messages are all bare types takes `MAX = 0`.
///
/// **A frame's handles come with it.** What the caller does not take with
/// [`take_handles`](Self::take_handles) is closed before the next frame is
/// taken or when this is dropped.
///
/// [`pump`]: Self::pump
/// [`recv`]: Self::recv
pub struct FrameRx<const MAX: usize> {
    msg_type: [u8; TYPE_LEN],
    payload: [u8; MAX],
    held: Batch,
}

impl<const MAX: usize> Default for FrameRx<MAX> {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a receive took no frame.
enum Refused {
    Malformed,
    Kernel(SyscallError),
}

impl Refused {
    fn blocking(self) -> IpcError {
        match self {
            // A frame larger than the receiver holds, or with more handles,
            // is the peer's: it stays queued, as `step` says.
            Refused::Malformed | Refused::Kernel(SyscallError::InvalidArgument) => IpcError::Malformed,
            Refused::Kernel(SyscallError::Gone) => IpcError::Disconnected,
            // The rest are this side's, a table without room for the frame's
            // handles (`ResourceExhausted`) among them.
            Refused::Kernel(e) => IpcError::Syscall(e),
        }
    }
}

impl<const MAX: usize> FrameRx<MAX> {
    pub const fn new() -> Self {
        Self { msg_type: [0; TYPE_LEN], payload: [0; MAX], held: Batch::EMPTY }
    }

    /// The payload of the frame the last [`RxStep::Frame`] announced.
    pub fn payload(&self, len: usize) -> &[u8] {
        &self.payload[..len]
    }

    /// Zero the payload buffer, so a secret a frame carried does not outlive its reading. The
    /// stores are volatile: a buffer about to be freed is still wiped.
    pub fn wipe(&mut self) {
        for byte in &mut self.payload {
            // SAFETY: a valid, exclusively borrowed `u8`.
            unsafe { core::ptr::write_volatile(byte, 0) };
        }
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    }

    /// The handles the last frame carried, now the caller's to claim.
    pub fn take_batch(&mut self) -> Batch {
        core::mem::replace(&mut self.held, Batch::EMPTY)
    }

    /// The `N` handles the last frame carried, now the caller's; `None`, with
    /// them closed, when it carried another number.
    pub fn take_handles<const N: usize>(&mut self) -> Option<[OwnedHandle; N]> {
        self.take_batch().exact::<N>()
    }

    /// Take the next frame if one is queued, never waiting: at most one per
    /// call, so a peer that always has another ready cannot keep a server's
    /// other peers from being served.
    pub fn pump(&mut self, conn: &impl AsHandle) -> RxStep {
        step(self.take(conn.as_handle(), true))
    }

    /// Wait for the next frame: [`IpcError::Disconnected`] once the peer has
    /// gone with nothing left, and [`IpcError::Malformed`] for one this
    /// receiver cannot take, which stays queued.
    pub fn recv(&mut self, conn: &impl AsHandle) -> Result<Frame, IpcError> {
        self.take(conn.as_handle(), false).map_err(Refused::blocking)
    }

    fn take(&mut self, conn: BorrowedHandle<'_>, nonblock: bool) -> Result<Frame, Refused> {
        self.held = Batch::EMPTY;
        let (frame, batch) = receive(conn, &mut self.msg_type, &mut self.payload, nonblock)?;
        self.held = batch;
        Ok(frame)
    }
}

/// One frame off `conn`: its type into `msg_type` and its payload into
/// `payload`, straight from the kernel, and the handles it carried.
fn receive(
    conn: BorrowedHandle<'_>,
    msg_type: &mut [u8; TYPE_LEN],
    payload: &mut [u8],
    nonblock: bool,
) -> Result<(Frame, Batch), Refused> {
    let mut handles = [toyos_abi::HANDLE_INVALID; syscall::MAX_TRANSFER_HANDLES];
    let got = syscall::message_recv(conn.raw(), msg_type, payload, &mut handles, nonblock);
    taken(got, *msg_type, handles)
}

/// A receive's answer, the handles it installed now the caller's.
fn taken(
    got: Result<Received, SyscallError>,
    msg_type: [u8; TYPE_LEN],
    handles: [RawHandle; syscall::MAX_TRANSFER_HANDLES],
) -> Result<(Frame, Batch), Refused> {
    let got = got.map_err(Refused::Kernel)?;
    // The kernel installed each number in this process's table for this
    // call, so this batch is the only thing that answers for them: dropped
    // with a malformed frame, it closes them.
    let batch = Batch { handles, next: 0, len: got.handles };
    let payload_len = got.len.checked_sub(TYPE_LEN).ok_or(Refused::Malformed)?;
    Ok((Frame { msg_type: u32::from_ne_bytes(msg_type), payload_len, handles: got.handles }, batch))
}

/// What a non-blocking receive's answer is to a server.
fn step(taken: Result<Frame, Refused>) -> RxStep {
    match taken {
        Ok(Frame { msg_type, payload_len, handles }) => RxStep::Frame { msg_type, payload_len, handles },
        Err(Refused::Kernel(SyscallError::WouldBlock)) => RxStep::Idle,
        Err(Refused::Malformed | Refused::Kernel(SyscallError::InvalidArgument | SyscallError::ResourceExhausted)) => {
            RxStep::Malformed
        }
        // The handle itself is gone, which is the same hang-up seen from
        // the other side of the same race.
        Err(Refused::Kernel(_)) => RxStep::Eof,
    }
}

/// The handles one frame carried, owned, handed out one at a time: what
/// nobody takes is closed when this drops.
pub struct Batch {
    handles: [RawHandle; syscall::MAX_TRANSFER_HANDLES],
    next: usize,
    len: usize,
}

impl Batch {
    const EMPTY: Batch = Batch { handles: [toyos_abi::HANDLE_INVALID; syscall::MAX_TRANSFER_HANDLES], next: 0, len: 0 };

    /// The handles not yet taken.
    pub fn len(&self) -> usize {
        self.len - self.next
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Exactly `N` handles; `None`, with them closed, for any other number.
    pub fn exact<const N: usize>(mut self) -> Option<[OwnedHandle; N]> {
        const { assert!(N <= syscall::MAX_TRANSFER_HANDLES) };
        if self.len() != N {
            return None;
        }
        Some(core::array::from_fn(|_| self.next().expect("counted above")))
    }
}

impl Iterator for Batch {
    type Item = OwnedHandle;

    fn next(&mut self) -> Option<OwnedHandle> {
        let raw = *self.handles[..self.len].get(self.next)?;
        self.next += 1;
        Some(OwnedHandle(raw))
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        self.by_ref().for_each(drop);
    }
}

/// A payload's bytes, as a frame carries them.
pub(crate) fn as_bytes<T: IpcPayload>(val: &T) -> &[u8] {
    unsafe { core::slice::from_raw_parts(val as *const T as *const u8, core::mem::size_of::<T>()) }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    /// A handle whose close is recorded rather than made.
    struct Probe {
        raw: RawHandle,
        closed: Rc<RefCell<Vec<u32>>>,
    }

    impl AsHandle for Probe {
        // SAFETY: a number no syscall is made on: these tests' sends are closures.
        fn as_handle(&self) -> BorrowedHandle<'_> { unsafe { BorrowedHandle::borrow_raw(self.raw) } }
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            self.closed.borrow_mut().push(self.raw.0);
        }
    }

    fn probes(count: u32, closed: &Rc<RefCell<Vec<u32>>>) -> Vec<Probe> {
        (1..=count).map(|n| Probe { raw: RawHandle(n), closed: Rc::clone(closed) }).collect()
    }

    /// A payload is one `T` only at exactly its width: one byte short or one
    /// byte over is refused.
    #[test]
    fn a_payload_decodes_only_at_exactly_its_width() {
        let wire = [1, 2, 3, 4, 5, 6, 7, 8, 9];
        let words: [u32; 2] = decode_payload(&wire[..8]).unwrap();
        assert_eq!(words, [u32::from_ne_bytes([1, 2, 3, 4]), u32::from_ne_bytes([5, 6, 7, 8])]);
        assert!(matches!(decode_payload::<[u32; 2]>(&wire[..7]), Err(IpcError::Malformed)));
        assert!(matches!(decode_payload::<[u32; 2]>(&wire), Err(IpcError::Malformed)));
        assert!(matches!(decode_payload::<[u32; 2]>(&[]), Err(IpcError::Malformed)));
    }

    /// A frame is its type's four bytes and then its payload, and nothing
    /// else of the buffer goes to the kernel.
    #[test]
    fn a_frame_is_its_type_then_exactly_its_payload() {
        let mut frame = core::mem::MaybeUninit::uninit();
        let bytes = framed(&mut frame, 0x0102_0304, &[9, 8, 7]).unwrap();
        let mut want = 0x0102_0304u32.to_ne_bytes().to_vec();
        want.extend([9, 8, 7]);
        assert_eq!(bytes, &want[..]);
        let mut frame = core::mem::MaybeUninit::uninit();
        assert_eq!(framed(&mut frame, 5, &[]).unwrap(), &5u32.to_ne_bytes());
    }

    /// The largest payload is framed, and one byte more is refused before
    /// the kernel is asked.
    #[test]
    fn a_payload_past_the_frame_bound_is_refused() {
        let most = vec![0u8; MAX_FRAME_LEN as usize];
        let mut frame = core::mem::MaybeUninit::uninit();
        assert_eq!(framed(&mut frame, 1, &most).map(<[u8]>::len), Some(TYPE_LEN + most.len()));
        let over = vec![0u8; MAX_FRAME_LEN as usize + 1];
        let mut frame = core::mem::MaybeUninit::uninit();
        assert!(framed(&mut frame, 1, &over).is_none());
    }

    /// A refused frame consumes its handles all the same.
    #[test]
    fn a_payload_too_large_closes_its_handles() {
        let closed = Rc::default();
        let over = vec![0u8; MAX_FRAME_LEN as usize + 1];
        // SAFETY: a number no syscall is made on: the frame is refused first.
        let conn = unsafe { BorrowedHandle::borrow_raw(RawHandle(77)) };
        assert!(matches!(message(conn, 1, &over, probes(2, &closed), false), Err(Unsent::TooLarge)));
        assert_eq!(*closed.borrow(), [1, 2]);
    }

    /// A message is its type and then its payload: one too short for a type
    /// is no frame, and the type is read from the front.
    #[test]
    fn a_message_shorter_than_a_type_is_malformed() {
        let none = [toyos_abi::HANDLE_INVALID; syscall::MAX_TRANSFER_HANDLES];
        let seven = 7u32.to_ne_bytes();
        assert!(matches!(taken(Ok(Received { len: TYPE_LEN - 1, handles: 0 }), seven, none), Err(Refused::Malformed)));
        let (frame, _) = taken(Ok(Received { len: TYPE_LEN, handles: 0 }), seven, none).ok().unwrap();
        assert_eq!(frame, Frame { msg_type: 7, payload_len: 0, handles: 0 });
        let (frame, _) = taken(Ok(Received { len: TYPE_LEN + 8, handles: 0 }), seven, none).ok().unwrap();
        assert_eq!(frame.payload_len, 8);
    }

    /// The kernel's refusals reach a server as the step it acts on.
    #[test]
    fn a_receive_refused_is_the_step_a_server_acts_on() {
        let none = [toyos_abi::HANDLE_INVALID; syscall::MAX_TRANSFER_HANDLES];
        for (e, idle, eof) in [
            (SyscallError::WouldBlock, true, false),
            (SyscallError::Gone, false, true),
            (SyscallError::InvalidArgument, false, false),
            (SyscallError::ResourceExhausted, false, false),
        ] {
            let step = step(taken(Err(e), [0; TYPE_LEN], none).map(|(frame, _)| frame));
            assert_eq!(matches!(step, RxStep::Idle), idle, "{e:?} answered {step:?}");
            assert_eq!(matches!(step, RxStep::Eof), eof, "{e:?} answered {step:?}");
        }
    }

    /// The same refusals reach a waiting client as the error it acts on: a
    /// frame too large for it is the peer's protocol broken, never a
    /// kernel fault.
    #[test]
    fn a_receive_refused_is_the_error_a_client_acts_on() {
        let none = [toyos_abi::HANDLE_INVALID; syscall::MAX_TRANSFER_HANDLES];
        let blocking = |got| taken(got, [0; TYPE_LEN], none).map(|(frame, _)| frame).map_err(Refused::blocking);
        assert!(matches!(blocking(Err(SyscallError::InvalidArgument)), Err(IpcError::Malformed)));
        assert!(matches!(blocking(Ok(Received { len: TYPE_LEN - 1, handles: 0 })), Err(IpcError::Malformed)));
        assert!(matches!(blocking(Err(SyscallError::Gone)), Err(IpcError::Disconnected)));
        assert!(matches!(
            blocking(Err(SyscallError::ResourceExhausted)),
            Err(IpcError::Syscall(SyscallError::ResourceExhausted))
        ));
    }

    /// The kernel's refusal leaves every handle at its own number, and the
    /// batch closes each of them, once.
    #[test]
    fn a_refused_move_closes_every_handle_it_consumed() {
        let closed = Rc::default();
        let mut seen = Vec::new();
        let sent = move_batch(probes(3, &closed), |batch| {
            seen.extend(batch.iter().map(|h| h.0));
            Err(SyscallError::Gone)
        });
        assert_eq!(sent, Err(SyscallError::Gone));
        assert_eq!(seen, [1, 2, 3], "the kernel was not asked to move the batch in order");
        assert_eq!(*closed.borrow(), [1, 2, 3]);
    }

    /// A taken batch is the peer's: closing a number this process no longer
    /// holds would end it.
    #[test]
    fn a_taken_move_closes_nothing() {
        let closed = Rc::default();
        assert_eq!(move_batch(probes(2, &closed), |_| Ok(())), Ok(()));
        assert!(closed.borrow().is_empty(), "closed {:?} after a move", closed.borrow());
    }

    /// One past the kernel's bound is refused before the kernel is asked, and
    /// every handle, the one that did not fit included, is closed.
    #[test]
    fn a_batch_past_the_bound_is_refused_and_closed() {
        let closed = Rc::default();
        let over = syscall::MAX_TRANSFER_HANDLES as u32 + 1;
        let sent = move_batch(probes(over, &closed), |_| panic!("an oversized batch reached the kernel"));
        assert_eq!(sent, Err(SyscallError::InvalidArgument));
        let mut closed = closed.borrow().clone();
        closed.sort_unstable();
        assert_eq!(closed, (1..=over).collect::<Vec<_>>());
    }
}
