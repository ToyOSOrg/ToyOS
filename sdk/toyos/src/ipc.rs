//! Framed IPC over connections (bidirectional pipe pairs).
//!
//! Wire format: `[u32 msg_type][u32 len][u32 handles][len bytes payload]`.
//!
//! **A frame and the handles it announces travel as one message**
//! (`SYS_MESSAGE_SEND`): the kernel writes the frame whole and pins its batch
//! at the byte just past it, so the receiver reads the frame and takes exactly
//! the batch it announces, there. A read that reaches a batch no frame
//! announced is answered `HandlesPending`, which is the peer breaking the
//! protocol; the connection is dropped and the batch goes with it.

use toyos_abi::RawHandle;
use toyos_abi::syscall::{self, SyscallError};
use crate::{AsHandle, OwnedHandle};

/// A type that may cross an IPC boundary as a payload.
///
/// # Safety
/// Implementors must be `#[repr(C)]`, contain no padding bytes, hold no
/// pointers, and have no invalid bit patterns. All three are load-bearing:
/// [`send`] publishes `size_of::<T>()` bytes of the sender's memory to a peer,
/// so a padding byte no field owns is whatever the sender's stack held; and
/// [`recv_payload`] reinterprets bytes a *peer* chose, so a `bool`, a
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
///
/// What the bound buys is a refusal in O(1). A peer's `len` sizes a
/// [`recv_bytes`] tail-skip, so `len = u32::MAX` obliges the reader to consume
/// every byte that peer will ever send before it returns to its event loop.
/// Bounded, that frame is an error at the header and the peer can be dropped.
pub const MAX_FRAME_LEN: u32 = 8192;

/// A frame header. `len` and `handles` are private because they carry
/// invariants a peer does not: every `IpcHeader` in existence has a length
/// within [`MAX_FRAME_LEN`] and a count within
/// [`MAX_TRANSFER_HANDLES`](syscall::MAX_TRANSFER_HANDLES), because
/// [`IpcHeader::from_wire`] is the only way to make one out of bytes somebody
/// else chose.
#[derive(Clone, Copy)]
pub struct IpcHeader {
    pub msg_type: u32,
    len: u32,
    handles: u32,
}

impl IpcHeader {
    pub const WIRE_SIZE: usize = 12;

    /// Build a header from words that have not been trusted yet.
    pub fn from_wire(msg_type: u32, len: u32, handles: u32) -> Result<Self, IpcError> {
        if len > MAX_FRAME_LEN || handles as usize > syscall::MAX_TRANSFER_HANDLES {
            return Err(IpcError::Malformed);
        }
        Ok(Self { msg_type, len, handles })
    }

    fn decode(bytes: &[u8; Self::WIRE_SIZE]) -> Result<Self, IpcError> {
        let word = |at: usize| u32::from_ne_bytes(bytes[at..at + 4].try_into().expect("four bytes"));
        Self::from_wire(word(0), word(4), word(8))
    }

    pub fn len(&self) -> u32 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// How many handles travel with this frame.
    pub fn handles(&self) -> usize {
        self.handles as usize
    }

    fn to_wire(self) -> [u8; Self::WIRE_SIZE] {
        let mut bytes = [0u8; Self::WIRE_SIZE];
        bytes[..4].copy_from_slice(&self.msg_type.to_ne_bytes());
        bytes[4..8].copy_from_slice(&self.len.to_ne_bytes());
        bytes[8..].copy_from_slice(&self.handles.to_ne_bytes());
        bytes
    }

    /// The same header, built from a length and count this endpoint chose itself.
    fn frame(msg_type: u32, len: usize, handles: usize) -> Result<Self, IpcError> {
        if len > MAX_FRAME_LEN as usize || handles > syscall::MAX_TRANSFER_HANDLES {
            return Err(IpcError::TooLarge);
        }
        Ok(Self { msg_type, len: len as u32, handles: handles as u32 })
    }
}

const _: () = assert!(IpcHeader::WIRE_SIZE + MAX_FRAME_LEN as usize <= syscall::MAX_MESSAGE_LEN);

/// The payloads framed in a buffer of their own size rather than one of
/// [`MAX_FRAME_LEN`]: every typed payload and signal, sent per input event.
const SMALL_PAYLOAD: usize = 64;

#[derive(Debug)]
pub enum IpcError {
    Disconnected,
    /// The peer's frame does not describe a message this endpoint can read:
    /// a length past [`MAX_FRAME_LEN`], or one other than the payload the
    /// caller asked for.
    Malformed,
    /// This endpoint will not frame a payload that large. Distinct from
    /// [`IpcError::Malformed`] because the two blame different sides.
    TooLarge,
    Syscall(SyscallError),
}

/// Why a whole frame could not be handed over without blocking.
#[derive(Debug)]
pub enum TrySendError {
    /// The peer's stream has no room for the whole frame now. **Nothing was
    /// sent**: a frame goes in whole or not at all. A peer that cannot take
    /// one frame has a stream of unread messages behind it, and is dropped.
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
    fn as_handle(&self) -> RawHandle { self.0.raw() }
}

impl Connection {
    /// A connection end a peer sent, consumed. Its type is the sender's
    /// word: the kernel refuses each connection call on a handle of another
    /// type, so trust the sender only as far as its protocol says.
    pub fn received(raw: RawHandle) -> Self {
        Self(OwnedHandle(raw))
    }

    /// This end as a handle, to send it on in a frame ([`Self::send_with`]).
    pub fn into_owned(self) -> OwnedHandle {
        self.0
    }

    pub fn send<T: IpcPayload>(&self, msg_type: u32, payload: &T) -> Result<(), IpcError> {
        send(self.0.raw(), msg_type, payload)
    }

    pub fn signal(&self, msg_type: u32) -> Result<(), IpcError> {
        signal(self.0.raw(), msg_type)
    }

    pub fn send_bytes(&self, msg_type: u32, data: &[u8]) -> Result<(), IpcError> {
        send_bytes(self.0.raw(), msg_type, data)
    }

    pub fn try_send<T: IpcPayload>(&self, msg_type: u32, payload: &T) -> Result<(), TrySendError> {
        try_send(self.0.raw(), msg_type, payload)
    }

    pub fn try_signal(&self, msg_type: u32) -> Result<(), TrySendError> {
        try_signal(self.0.raw(), msg_type)
    }

    pub fn try_send_bytes(&self, msg_type: u32, data: &[u8]) -> Result<(), TrySendError> {
        try_send_bytes(self.0.raw(), msg_type, data)
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
        message(self.0.raw(), msg_type, as_bytes(payload), handles, false).map_err(blocking)
    }

    /// [`send_with`](Self::send_with) for a byte payload.
    pub fn send_bytes_with(
        &self,
        msg_type: u32,
        data: &[u8],
        handles: impl IntoIterator<Item = OwnedHandle>,
    ) -> Result<(), IpcError> {
        message(self.0.raw(), msg_type, data, handles, false).map_err(blocking)
    }

    /// [`send_with`](Self::send_with) for a bare header.
    pub fn signal_with(&self, msg_type: u32, handles: impl IntoIterator<Item = OwnedHandle>) -> Result<(), IpcError> {
        message(self.0.raw(), msg_type, &[], handles, false).map_err(blocking)
    }

    /// [`send_with`](Self::send_with) without blocking: nothing is sent and
    /// every handle is closed when the frame does not fit now.
    pub fn try_send_with<T: IpcPayload>(
        &self,
        msg_type: u32,
        payload: &T,
        handles: impl IntoIterator<Item = OwnedHandle>,
    ) -> Result<(), TrySendError> {
        message(self.0.raw(), msg_type, as_bytes(payload), handles, true).map_err(nonblocking)
    }

    /// [`try_send_with`](Self::try_send_with) for a byte payload.
    pub fn try_send_bytes_with(
        &self,
        msg_type: u32,
        data: &[u8],
        handles: impl IntoIterator<Item = OwnedHandle>,
    ) -> Result<(), TrySendError> {
        message(self.0.raw(), msg_type, data, handles, true).map_err(nonblocking)
    }

    /// [`try_send_with`](Self::try_send_with) for a bare header.
    pub fn try_signal_with(
        &self,
        msg_type: u32,
        handles: impl IntoIterator<Item = OwnedHandle>,
    ) -> Result<(), TrySendError> {
        message(self.0.raw(), msg_type, &[], handles, true).map_err(nonblocking)
    }

    pub fn recv_header(&self) -> Result<IpcHeader, IpcError> {
        recv_header(self.0.raw())
    }

    pub fn recv_payload<T: IpcPayload>(&self, header: &IpcHeader) -> Result<T, IpcError> {
        recv_payload(self.0.raw(), header)
    }

    pub fn recv<T: IpcPayload>(&self) -> Result<(u32, T), IpcError> {
        recv(self.0.raw())
    }

    /// The handles `header`'s frame announced, read after its payload: exactly
    /// that many, or [`IpcError::Malformed`] with whatever arrived closed. The
    /// kernel pins a frame's batch just past its bytes, so they are there.
    pub fn recv_handles(
        &self,
        header: &IpcHeader,
        out: &mut [RawHandle; syscall::MAX_TRANSFER_HANDLES],
    ) -> Result<usize, IpcError> {
        recv_handles(self.0.raw(), header, out)
    }

    /// The `N` handles a frame of this message type carries.
    ///
    /// `None` for any other count, with whatever did arrive closed rather than
    /// leaked: a frame announcing another number speaks a protocol this
    /// endpoint does not.
    pub fn recv_handles_exact<const N: usize>(&self, header: &IpcHeader) -> Option<[RawHandle; N]> {
        const { assert!(N <= syscall::MAX_TRANSFER_HANDLES) };
        let mut batch = [toyos_abi::HANDLE_INVALID; syscall::MAX_TRANSFER_HANDLES];
        let n = self.recv_handles(header, &mut batch).ok()?;
        if n != N {
            close_all(&batch[..n]);
            return None;
        }
        Some(core::array::from_fn(|i| batch[i]))
    }

    pub fn recv_bytes(&self, header: &IpcHeader, buf: &mut [u8]) -> Result<usize, IpcError> {
        recv_bytes(self.0.raw(), header, buf)
    }

    pub fn read_nonblock(&self, buf: &mut [u8]) -> Result<usize, SyscallError> {
        self.0.read_nonblock(buf)
    }

    /// Raw bytes, for a connection a protocol has made a doorbell: never a
    /// frame, which [`send`](Self::send) and its kin write whole.
    pub fn write_nonblock(&self, buf: &[u8]) -> Result<usize, SyscallError> {
        self.0.write_nonblock(buf)
    }
}

// Free functions — used by consumers that hold raw handles (compositor, netstack).
// Will become pub(crate) once all callers migrate to Connection methods.

pub fn send<T: IpcPayload>(handle: RawHandle, msg_type: u32, payload: &T) -> Result<(), IpcError> {
    message(handle, msg_type, as_bytes(payload), none(), false).map_err(blocking)
}

pub fn signal(handle: RawHandle, msg_type: u32) -> Result<(), IpcError> {
    message(handle, msg_type, &[], none(), false).map_err(blocking)
}

pub fn send_bytes(handle: RawHandle, msg_type: u32, data: &[u8]) -> Result<(), IpcError> {
    message(handle, msg_type, data, none(), false).map_err(blocking)
}

/// Send a framed payload without ever blocking, or refuse the whole frame.
///
/// A server writing to a client it does not trust to read cannot use [`send`]:
/// a blocking send parks in the kernel until the peer drains, which is a
/// client deciding when the server runs again. This is the same frame, and the
/// peer's backlog is an answer rather than a wait.
pub fn try_send<T: IpcPayload>(handle: RawHandle, msg_type: u32, payload: &T) -> Result<(), TrySendError> {
    message(handle, msg_type, as_bytes(payload), none(), true).map_err(nonblocking)
}

/// [`signal`] without blocking.
pub fn try_signal(handle: RawHandle, msg_type: u32) -> Result<(), TrySendError> {
    message(handle, msg_type, &[], none(), true).map_err(nonblocking)
}

/// [`send_bytes`] without blocking.
pub fn try_send_bytes(handle: RawHandle, msg_type: u32, data: &[u8]) -> Result<(), TrySendError> {
    message(handle, msg_type, data, none(), true).map_err(nonblocking)
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

/// The one way a frame leaves this endpoint: its header and `payload` in one
/// buffer, and `handles`, handed to the kernel as one message. Every handle is
/// consumed whatever the answer.
fn message<H: AsHandle>(
    handle: RawHandle,
    msg_type: u32,
    payload: &[u8],
    handles: impl IntoIterator<Item = H>,
    nonblock: bool,
) -> Result<(), Unsent> {
    let mut too_large = false;
    let sent = move_batch(handles, |batch| {
        let Ok(header) = IpcHeader::frame(msg_type, payload.len(), batch.len()) else {
            too_large = true;
            return Err(SyscallError::InvalidArgument);
        };
        let send = |frame: &mut [u8]| {
            let end = IpcHeader::WIRE_SIZE + payload.len();
            frame[..IpcHeader::WIRE_SIZE].copy_from_slice(&header.to_wire());
            frame[IpcHeader::WIRE_SIZE..end].copy_from_slice(payload);
            syscall::message_send(handle, &frame[..end], batch, nonblock)
        };
        match payload.len() <= SMALL_PAYLOAD {
            true => send(&mut [0u8; IpcHeader::WIRE_SIZE + SMALL_PAYLOAD]),
            false => send(&mut [0u8; IpcHeader::WIRE_SIZE + MAX_FRAME_LEN as usize]),
        }
    });
    match sent {
        Ok(()) => Ok(()),
        Err(_) if too_large => Err(Unsent::TooLarge),
        Err(e) => Err(Unsent::Kernel(e)),
    }
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
        *out = handle.as_handle();
    }
    send(&raw[..count])?;
    #[expect(clippy::disallowed_methods, reason = "taken: the numbers are no longer this process's to close")]
    batch.into_iter().flatten().for_each(core::mem::forget);
    Ok(())
}

pub fn recv_header(handle: RawHandle) -> Result<IpcHeader, IpcError> {
    let mut bytes = [0u8; IpcHeader::WIRE_SIZE];
    read_exact(handle, &mut bytes)?;
    IpcHeader::decode(&bytes)
}

/// The frame's payload as one `T`. A payload of any other length is read
/// past, so the connection stays framed, and refused [`IpcError::Malformed`].
pub fn recv_payload<T: IpcPayload>(handle: RawHandle, header: &IpcHeader) -> Result<T, IpcError> {
    if let Err(e) = exactly::<T>(header.len as usize) {
        skip(handle, header.len as usize)?;
        return Err(e);
    }
    let mut val = core::mem::MaybeUninit::<T>::uninit();
    // SAFETY: the slice covers exactly the `T` being filled, and `IpcPayload`
    // promises every bit pattern `read_exact` can write is a valid `T`.
    read_exact(handle, unsafe {
        core::slice::from_raw_parts_mut(val.as_mut_ptr() as *mut u8, core::mem::size_of::<T>())
    })?;
    Ok(unsafe { val.assume_init() })
}

/// Receive header + typed payload in one call, for a frame that carries no
/// handles: one that does is [`IpcError::Malformed`], its batch closed.
pub fn recv<T: IpcPayload>(handle: RawHandle) -> Result<(u32, T), IpcError> {
    let header = recv_header(handle)?;
    let payload = recv_payload(handle, &header)?;
    if header.handles() != 0 {
        let mut stray = [toyos_abi::HANDLE_INVALID; syscall::MAX_TRANSFER_HANDLES];
        let _ = recv_handles(handle, &header, &mut stray).map(|n| close_all(&stray[..n]));
        return Err(IpcError::Malformed);
    }
    Ok((header.msg_type, payload))
}

/// Receive raw bytes. Returns the number of valid bytes read.
pub fn recv_bytes(handle: RawHandle, header: &IpcHeader, buf: &mut [u8]) -> Result<usize, IpcError> {
    let count = (header.len as usize).min(buf.len());
    if count > 0 {
        read_exact(handle, &mut buf[..count])?;
    }
    skip(handle, header.len as usize - count)?;
    Ok(count)
}

/// The batch `header` announced, taken where the kernel pinned it: exactly
/// [`IpcHeader::handles`] of them, or [`IpcError::Malformed`] with any that
/// arrived closed.
pub fn recv_handles(
    handle: RawHandle,
    header: &IpcHeader,
    out: &mut [RawHandle; syscall::MAX_TRANSFER_HANDLES],
) -> Result<usize, IpcError> {
    let want = header.handles();
    if want == 0 {
        return Ok(0);
    }
    let n = syscall::handle_recv(handle, out).map_err(IpcError::Syscall)?;
    if n != want {
        close_all(&out[..n]);
        return Err(IpcError::Malformed);
    }
    Ok(n)
}

fn close_all(handles: &[RawHandle]) {
    for h in handles {
        syscall::close(*h);
    }
}

/// The memory twin of [`recv_payload`], for a reader that already holds the
/// bytes.
///
/// Buffering a whole frame before acting on it is what lets a server stay
/// non-blocking against a peer that stops mid-message, and a buffered payload
/// has no handle left to read it from.
pub fn decode_payload<T: IpcPayload>(bytes: &[u8]) -> Result<T, IpcError> {
    exactly::<T>(bytes.len())?;
    let mut val = core::mem::MaybeUninit::<T>::uninit();
    // SAFETY: the copy fills exactly the `T` being built from a source that
    // long, and `IpcPayload` promises every bit pattern is a valid `T` — the
    // same contract `recv_payload` reads on.
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), val.as_mut_ptr() as *mut u8, bytes.len());
        Ok(val.assume_init())
    }
}

/// A typed payload is exactly one `T`: a shorter one is not a `T`, and the
/// bytes past a longer one are no protocol's.
fn exactly<T: IpcPayload>(len: usize) -> Result<(), IpcError> {
    if len == core::mem::size_of::<T>() { Ok(()) } else { Err(IpcError::Malformed) }
}

/// What one [`FrameRx::pump`] found.
#[derive(Clone, Copy, Debug)]
pub enum RxStep {
    /// The handle has nothing more right now.
    Idle,
    /// A whole frame is buffered; its payload is [`FrameRx::payload`] and its
    /// `handles` are [`FrameRx::take_handles`]'s.
    Frame { msg_type: u32, payload_len: usize, handles: usize },
    /// The peer hung up, or its handle is gone.
    Eof,
    /// The peer broke framing: a length past [`MAX_FRAME_LEN`] or the
    /// receiver's `MAX`, a count past the bound, a batch other than its frame
    /// announced, or one no frame announced. Nothing after it can be located,
    /// so there is nothing to resynchronise to.
    Malformed,
}

/// One peer's inbound framing, over non-blocking reads only.
///
/// **A server never reads a client with a blocking read.** That is the whole
/// point of this type: [`recv_header`] and [`recv_payload`] park the caller
/// until the peer sends the bytes it promised, which hands a client the
/// decision of when the server runs again. Here a peer that stops halfway
/// through a frame costs a buffer instead of the event loop.
///
/// `MAX` is the widest payload the caller's protocol carries. A frame
/// declaring more is [`RxStep::Malformed`] at its header, so a peer cannot
/// make the server hold what it will not look at, and no frame arrives cut
/// short. A protocol whose messages are all bare headers takes `MAX = 0`.
///
/// **A frame's handles come with it.** They are taken when the frame is, and
/// what the caller does not take with [`take_handles`](Self::take_handles) is
/// closed before the next frame is read or when this is dropped.
pub struct FrameRx<const MAX: usize> {
    header: [u8; IpcHeader::WIRE_SIZE],
    buf: [u8; MAX],
    len: usize,
    state: RxState,
    held: [RawHandle; syscall::MAX_TRANSFER_HANDLES],
    held_len: usize,
}

enum RxState {
    Header,
    Payload { msg_type: u32, len: usize, handles: usize },
}

impl<const MAX: usize> Default for FrameRx<MAX> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const MAX: usize> Drop for FrameRx<MAX> {
    fn drop(&mut self) {
        self.release();
    }
}

impl<const MAX: usize> FrameRx<MAX> {
    pub const fn new() -> Self {
        Self {
            header: [0; IpcHeader::WIRE_SIZE],
            buf: [0; MAX],
            len: 0,
            state: RxState::Header,
            held: [toyos_abi::HANDLE_INVALID; syscall::MAX_TRANSFER_HANDLES],
            held_len: 0,
        }
    }

    /// The payload of the frame the last [`RxStep::Frame`] announced.
    pub fn payload(&self, len: usize) -> &[u8] {
        &self.buf[..len]
    }

    /// The handles the last frame carried, now the caller's to claim.
    pub fn take_batch(&mut self) -> Batch {
        let batch = Batch { handles: self.held, len: self.held_len };
        self.held_len = 0;
        batch
    }

    /// The `N` handles the last frame carried, now the caller's; `None`, with
    /// them closed, when it carried another number.
    pub fn take_handles<const N: usize>(&mut self) -> Option<[RawHandle; N]> {
        self.take_batch().exact::<N>()
    }

    fn release(&mut self) {
        close_all(&self.held[..self.held_len]);
        self.held_len = 0;
    }

    /// Read what is there, and report the first thing that happened.
    ///
    /// At most one frame per call: the caller has other peers to serve, and a
    /// peer that always has another frame ready must not be able to keep the
    /// rest from being served.
    pub fn pump(&mut self, conn: &Connection) -> RxStep {
        self.release();
        loop {
            match self.state {
                RxState::Header => {
                    match fill(conn, &mut self.header, &mut self.len) {
                        Fill::Idle => return RxStep::Idle,
                        Fill::Eof => return RxStep::Eof,
                        Fill::Malformed => return RxStep::Malformed,
                        Fill::Filled => {}
                    }
                    let Some((msg_type, len, handles)) = Self::announced(&self.header) else {
                        return RxStep::Malformed;
                    };
                    self.len = 0;
                    self.state = RxState::Payload { msg_type, len, handles };
                }
                RxState::Payload { msg_type, len, handles } => {
                    match fill(conn, &mut self.buf[..len], &mut self.len) {
                        Fill::Idle => return RxStep::Idle,
                        Fill::Eof => return RxStep::Eof,
                        Fill::Malformed => return RxStep::Malformed,
                        Fill::Filled => {}
                    }
                    self.len = 0;
                    self.state = RxState::Header;
                    if handles > 0 {
                        // Pinned just past the bytes now read, so it is there or never sent.
                        let n = syscall::handle_recv(conn.as_handle(), &mut self.held).unwrap_or(0);
                        self.held_len = n;
                        if n != handles {
                            return RxStep::Malformed;
                        }
                    }
                    return RxStep::Frame { msg_type, payload_len: len, handles };
                }
            }
        }
    }

    /// The message type, payload length and handle count `header` announces,
    /// or `None` for a frame this receiver cannot hold whole.
    fn announced(header: &[u8; IpcHeader::WIRE_SIZE]) -> Option<(u32, usize, usize)> {
        let header = IpcHeader::decode(header).ok()?;
        let len = header.len() as usize;
        (len <= MAX).then_some((header.msg_type, len, header.handles()))
    }
}

/// The handles one frame carried, owned: what nobody claims is closed when
/// this drops.
pub struct Batch {
    handles: [RawHandle; syscall::MAX_TRANSFER_HANDLES],
    len: usize,
}

impl Batch {
    /// Exactly `N` handles, now the caller's; `None`, with them closed, for
    /// any other number.
    pub fn exact<const N: usize>(mut self) -> Option<[RawHandle; N]> {
        const { assert!(N <= syscall::MAX_TRANSFER_HANDLES) };
        if self.len != N {
            return None;
        }
        self.len = 0;
        Some(core::array::from_fn(|i| self.handles[i]))
    }

    /// Every handle, as numbers now the caller's to own and close.
    pub fn into_raw(mut self) -> ([RawHandle; syscall::MAX_TRANSFER_HANDLES], usize) {
        let len = core::mem::take(&mut self.len);
        (self.handles, len)
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        close_all(&self.handles[..self.len]);
    }
}

enum Fill {
    Filled,
    Idle,
    Eof,
    /// A batch no frame announced sits where the next byte would be.
    Malformed,
}

/// Fill `buf` from `*len` onwards without blocking, carrying `*len` forward so
/// the next call resumes where this one stopped.
fn fill(conn: &Connection, buf: &mut [u8], len: &mut usize) -> Fill {
    while *len < buf.len() {
        match conn.read_nonblock(&mut buf[*len..]) {
            Ok(0) => return Fill::Eof,
            Ok(n) => *len += n,
            Err(SyscallError::WouldBlock) => return Fill::Idle,
            Err(SyscallError::HandlesPending) => return Fill::Malformed,
            // The handle itself is gone, which is the same hang-up seen from the
            // other side of the same race.
            Err(_) => return Fill::Eof,
        }
    }
    Fill::Filled
}

fn as_bytes<T: IpcPayload>(val: &T) -> &[u8] {
    unsafe { core::slice::from_raw_parts(val as *const T as *const u8, core::mem::size_of::<T>()) }
}

fn skip(handle: RawHandle, mut remaining: usize) -> Result<(), IpcError> {
    let mut buf = [0u8; 128];
    while remaining > 0 {
        let chunk = remaining.min(buf.len());
        read_exact(handle, &mut buf[..chunk])?;
        remaining -= chunk;
    }
    Ok(())
}

/// Fill `buf` with blocking reads. A batch no frame announced where the next
/// byte would be is the peer breaking framing.
fn read_exact(handle: RawHandle, buf: &mut [u8]) -> Result<(), IpcError> {
    let mut offset = 0;
    while offset < buf.len() {
        let n = syscall::read(handle, &mut buf[offset..]).map_err(|e| match e {
            SyscallError::HandlesPending => IpcError::Malformed,
            e => IpcError::Syscall(e),
        })?;
        if n == 0 {
            return Err(IpcError::Disconnected);
        }
        offset += n;
    }
    Ok(())
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
        fn as_handle(&self) -> RawHandle { self.raw }
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

    fn header(len: u32, handles: u32) -> [u8; IpcHeader::WIRE_SIZE] {
        let mut bytes = [0u8; IpcHeader::WIRE_SIZE];
        bytes[..4].copy_from_slice(&7u32.to_ne_bytes());
        bytes[4..8].copy_from_slice(&len.to_ne_bytes());
        bytes[8..].copy_from_slice(&handles.to_ne_bytes());
        bytes
    }

    /// A frame is held whole up to the receiver's `MAX` and refused at its
    /// header past it, never cut down to what fits.
    #[test]
    fn a_frame_past_the_receivers_max_is_refused_at_its_header() {
        assert_eq!(FrameRx::<8>::announced(&header(0, 0)), Some((7, 0, 0)));
        assert_eq!(FrameRx::<8>::announced(&header(8, 0)), Some((7, 8, 0)));
        assert_eq!(FrameRx::<8>::announced(&header(9, 0)), None);
        assert_eq!(FrameRx::<0>::announced(&header(1, 0)), None);
        assert_eq!(FrameRx::<{ MAX_FRAME_LEN as usize + 1 }>::announced(&header(MAX_FRAME_LEN + 1, 0)), None);
    }

    /// A frame announces at most the batch one message can carry: one more
    /// is refused at its header, as the kernel would refuse the send.
    #[test]
    fn a_frame_announcing_more_handles_than_a_message_carries_is_refused() {
        let most = syscall::MAX_TRANSFER_HANDLES as u32;
        assert_eq!(FrameRx::<8>::announced(&header(4, most)), Some((7, 4, most as usize)));
        assert_eq!(FrameRx::<8>::announced(&header(4, most + 1)), None);
        assert!(matches!(IpcHeader::frame(7, 0, most as usize + 1), Err(IpcError::TooLarge)));
        let built = IpcHeader::frame(7, 3, 2).unwrap().to_wire();
        let back = IpcHeader::decode(&built).unwrap();
        assert_eq!((back.msg_type, back.len(), back.handles()), (7, 3, 2));
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
