mod arch;
pub mod display;
pub mod framebuffer;
pub mod scale;
pub mod wait;
pub mod watcher;

pub use framebuffer::{Color, Framebuffer, Screen};
pub use scale::Scale;
pub use wait::{Waiter, Waker, Woke};
/// What [`Window::handle`] answers with, and what a [`Waiter`] waits on.
pub use toyos::BorrowedHandle;
/// A [`BorrowedHandle`]'s number.
pub use toyos_abi::RawHandle;
use toyos_abi::syscall::SyscallError;

use toyos::ipc;
use toyos::AsHandle;
use toyos::poller::{Poller, READABLE};
use toyos::endow::{self, EndowError};
use toyos::surface;
use toyos::Connection;
use toyos::shm::{Lent, ReadOnly, SharedMemory};
use std::rc::Rc;
/// Re-exported because [`KeyPress`] is made of them and a client that holds
/// its own translator — `console`, a test that stands in for a surface —
/// should not have to name a second crate to do what this one does.
pub use toyos_keymap::{Emit, Mods, Translator};

// Window flags
pub const WINDOW_FLAG_TOPMOST: u8 = 1;
/// [`CreateWindowRequest`]'s size is in logical pixels, which the compositor
/// converts at the output's scale; without it the size is physical.
pub const WINDOW_FLAG_LOGICAL_SIZE: u8 = 2;

/// The compositor's port every window but a prompt is asked for on.
const COMPOSITOR: &str = "compositor";

/// The largest payload a client sends inline; past it a region moves instead.
///
/// One number both ends read: an `ipc::FrameRx` refuses a frame declaring
/// more than its receiver holds, so a sender holding the larger of two such
/// numbers is dropped.
pub const MAX_INLINE_PAYLOAD: usize = 4096;

// Client → Compositor
pub const MSG_CREATE_WINDOW: u32 = 1;
pub const MSG_PRESENT: u32 = 2;
pub const MSG_CLIPBOARD_SET: u32 = 3;
pub const MSG_DESTROY_WINDOW: u32 = 4;
pub const MSG_SET_CURSOR: u32 = 5;

/// Cursor styles. A style the compositor does not implement — 0 among them —
/// is the default cursor rather than an index into anything
/// (`compositor::session::cursor_from_wire`), so the default has no name here:
/// it is every value these two are not.
pub const CURSOR_CROSSHAIR: u8 = 1;
pub const CURSOR_RESIZE: u8 = 2;

// Compositor → Client
pub const MSG_WINDOW_CREATED: u32 = 1;
pub const MSG_KEY_INPUT: u32 = 2;
pub const MSG_WINDOW_RESIZED: u32 = 3;
pub const MSG_WINDOW_CLOSE: u32 = 4;
pub const MSG_MOUSE_INPUT: u32 = 5;
pub const MSG_CLIPBOARD_PASTE: u32 = 6;
pub const MSG_FRAME: u32 = 7;
/// The compositor will not create the window that was asked for. Payload is a
/// [`WindowRefused`]. The only server→client message that answers
/// `MSG_CREATE_WINDOW` other than [`MSG_WINDOW_CREATED`], and the reason
/// [`Window::create`] is fallible: without it a compositor that cannot afford
/// another window has no move except to serve it or to drop the connection.
pub const MSG_WINDOW_REFUSED: u32 = 9;

/// Client → compositor, retired: it carried a region the client chose, and the
/// compositor never uses a handle a client sent. A client sending it is dropped
/// by name.
pub const MSG_RETIRED_CLIPBOARD_SET_SHM: u32 = 10;
/// Compositor → client: a clipboard past [`MAX_INLINE_PAYLOAD`], in a region
/// the compositor made. Payload is a [`ClipboardShmMsg`].
pub const MSG_CLIPBOARD_PASTE_SHM: u32 = 11;

/// Client → compositor, as a connection's first frame whose payload is exactly
/// a [`ClipboardShmMsg`]: a clipboard of that many bytes, which [`copy_fits`].
/// Answered with [`MSG_COPY_REGION`].
pub const MSG_COPY_BEGIN: u32 = 13;
/// Client → compositor, bare, and the only frame a connection answered with
/// [`MSG_COPY_REGION`] may send next: the text is in the region. The compositor
/// copies it once and closes the connection.
pub const MSG_COPY_COMMIT: u32 = 14;
/// Compositor → client, bare: a region the compositor made, as long as the copy
/// [`MSG_COPY_BEGIN`] announced, carried in the frame.
pub const MSG_COPY_REGION: u32 = 13;
/// Watcher → compositor, bare: the first frame on the clipboard port, and the
/// ask for the next copy ([`watcher`]). Answered, once a client has copied,
/// with the clipboard in the paste's two encodings, [`MSG_CLIPBOARD_PASTE`] or
/// [`MSG_CLIPBOARD_PASTE_SHM`], empty included.
pub const MSG_CLIPBOARD_WATCH: u32 = 15;
/// Compositor → watcher, bare: the answer to its [`MSG_CLIPBOARD_SET`] or its
/// [`MSG_COPY_COMMIT`], whether or not the set landed. A watcher has one set
/// in flight until it is answered.
pub const MSG_CLIPBOARD_SET_ANSWERED: u32 = 14;

/// The longest clipboard. Policy: a clipboard is text somebody selected.
pub const MAX_CLIPBOARD_BYTES: usize = 2 * 1024 * 1024;

/// Whether a clipboard of `len` bytes is one the compositor makes a region for.
pub fn copy_fits(len: usize) -> bool {
    (1..=MAX_CLIPBOARD_BYTES).contains(&len)
}

/// Either direction: [`surface::LAYOUT_CONFIG`] changed, re-read it.
///
/// The compositor translates nothing, so it does not act on this — it is the
/// root of the surface tree and its job is to give every window the same
/// answer. A client sends it after writing the config; the compositor sends it
/// to every window it holds.
pub const MSG_LAYOUT_CHANGED: u32 = 12;

/// Wire reasons carried by [`MSG_WINDOW_REFUSED`]. A client that does not know
/// a reason still knows it was refused, so adding one is backwards compatible
/// with an older client — [`CreateError::Refused`] carries the raw value.
pub const REFUSED_AT_CAPACITY: u32 = 1;
pub const REFUSED_TOO_LARGE: u32 = 2;
/// The compositor could not get the memory for the buffer. Distinct from
/// [`REFUSED_AT_CAPACITY`], which is the compositor's own budget saying no
/// while the machine still has memory.
pub const REFUSED_NO_MEMORY: u32 = 3;

toyos::ipc_payload! {
    pub struct WindowRefused {
        pub reason: u32,
    }
}

/// Why a request to the compositor failed: a window, or the exchange under a
/// [`CopyError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateError {
    /// This program was given no `compositor` connector. A statement about
    /// what the manifest says this program holds, not about the machine — and
    /// never about timing: every port exists before any server runs.
    NotEndowed,
    /// The compositor exited: its port is closed for good.
    CompositorGone,
    /// The kernel refused a call of the exchange: the connection or a frame.
    Kernel(SyscallError),
    /// The region the compositor sent is refused ([`SharedMemory::lent`]).
    Lent(Lent),
    /// The exchange broke off: the compositor closed the connection, having
    /// exited or refused (its log says which), or sent a frame this client
    /// cannot read.
    BrokenOff,
    /// The compositor answered, and not with anything this exchange allows.
    Protocol(u32),
    /// The answer came without the one handle it carries: the compositor sent
    /// none, or this process had no room to take it.
    NoHandle,
    /// The compositor is already holding as many windows as it can afford.
    AtCapacity,
    /// The requested size is bigger than the screen it would be drawn on.
    TooLarge,
    /// The machine has no memory for a buffer this size.
    NoMemory,
    /// Refused for a reason this build of the client does not know.
    Refused(u32),
}

impl std::fmt::Display for CreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotEndowed => write!(f, "this program was given no compositor"),
            Self::CompositorGone => write!(f, "the compositor is gone"),
            Self::Kernel(e) => {
                write!(f, "the kernel refused the exchange with the compositor ({e:?})")
            }
            Self::Lent(why) => write!(f, "the compositor's region is refused: {why}"),
            Self::BrokenOff => write!(f, "the compositor broke off the exchange"),
            Self::NoHandle => write!(f, "the compositor's answer arrived without its handle"),
            Self::AtCapacity => write!(f, "the compositor is at its window limit"),
            Self::TooLarge => write!(f, "the window is larger than the screen"),
            Self::NoMemory => write!(f, "there is no memory for a window that size"),
            Self::Refused(reason) => write!(f, "the compositor refused (reason {reason})"),
            Self::Protocol(msg_type) => {
                write!(f, "the compositor's answer (type {msg_type}) is not one this exchange allows")
            }
        }
    }
}

impl std::error::Error for CreateError {}

impl From<EndowError> for CreateError {
    fn from(e: EndowError) -> Self {
        match e {
            EndowError::NotEndowed => Self::NotEndowed,
            EndowError::ServerGone => Self::CompositorGone,
            EndowError::Refused(e) => Self::Kernel(e),
        }
    }
}

impl From<ipc::IpcError> for CreateError {
    fn from(e: ipc::IpcError) -> Self {
        match e {
            ipc::IpcError::Disconnected | ipc::IpcError::Malformed => Self::BrokenOff,
            ipc::IpcError::Syscall(e) => Self::Kernel(e),
            ipc::IpcError::TooLarge => {
                unreachable!("every frame this crate sends is within ipc::MAX_FRAME_LEN")
            }
        }
    }
}

const _: () = assert!(MAX_INLINE_PAYLOAD <= ipc::MAX_FRAME_LEN as usize);

impl CreateError {
    fn from_wire(reason: u32) -> Self {
        match reason {
            REFUSED_AT_CAPACITY => Self::AtCapacity,
            REFUSED_TOO_LARGE => Self::TooLarge,
            REFUSED_NO_MEMORY => Self::NoMemory,
            other => Self::Refused(other),
        }
    }
}

toyos::ipc_payload! {
    /// A rectangle of a surface, in that surface's own pixels: what a
    /// client claims it changed ([`PresentRect`]).
    pub struct Rect {
        pub x: u32,
        pub y: u32,
        pub w: u32,
        pub h: u32,
    }

    /// The payload of [`MSG_PRESENT`]: the client's claim about which of its
    /// pixels changed, and the generation of the buffer it drew them into,
    /// as [`WindowInfo`] or [`ResizeInfo`] gave it. A compositor that has to
    /// assume the whole window changed repaints the whole window for one
    /// typed character — which is what this exists to stop — so the claim is
    /// load-bearing and the compositor clamps it to the window rather than
    /// trusting it. The generation is what tells the compositor a present
    /// was drawn into a buffer it gave since, which it then shows.
    pub struct PresentRect {
        pub x: u32,
        pub y: u32,
        pub w: u32,
        pub h: u32,
        pub generation: u32,
    }

    /// How many bytes of clipboard text a copy region holds. Where a region
    /// travels with the message, it is one handle carried in the frame.
    pub struct ClipboardShmMsg {
        pub len: u32,
    }

    pub struct CreateWindowRequest {
        pub width: u32,
        pub height: u32,
        pub flags: u8,
        pub title_len: u8,
        pub title: [u8; 30],
    }
}

impl Rect {
    pub fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x + self.w).max(other.x + other.w);
        let bottom = (self.y + self.h).max(other.y + other.h);
        Self { x, y, w: right - x, h: bottom - y }
    }
}

pub const MOUSE_MOVE: u8 = 0;
pub const MOUSE_PRESS: u8 = 1;
pub const MOUSE_RELEASE: u8 = 2;
pub const MOUSE_SCROLL: u8 = 3;

toyos::ipc_payload! {
    pub struct MouseEvent {
        pub x: u16,
        pub y: u16,
        pub buttons: u8,
        pub event_type: u8,
        pub changed: u8,
        pub scroll: i8,
    }

    /// The shape of the window's buffer, in physical pixels, the scale to
    /// draw into it at, as [`Scale::wire`], and its generation, which every
    /// present drawn into it carries. **The buffer is one transferred
    /// handle**, carried in the frame that carries this.
    pub struct WindowInfo {
        pub width: u32,
        pub height: u32,
        pub stride: u32,
        pub pixel_format: u32,
        pub scale: u32,
        pub generation: u32,
    }

    /// The shape of the window's new buffer and its scale, which arrive as a
    /// handle the same way [`WindowInfo`]'s do: the size or the scale
    /// changed.
    pub struct ResizeInfo {
        pub width: u32,
        pub height: u32,
        pub stride: u32,
        pub pixel_format: u32,
        pub scale: u32,
        pub generation: u32,
    }
}

/// What size a window is asked for in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// Whatever the compositor gives it.
    Any,
    /// Logical pixels, which the compositor converts at the output's scale.
    Logical { width: u32, height: u32 },
    /// Physical pixels, as they are.
    Physical { width: u32, height: u32 },
}

impl Size {
    /// The request's size and its flag.
    fn wire(self) -> (u32, u32, u8) {
        match self {
            Self::Any => (0, 0, 0),
            Self::Logical { width, height } => (width, height, WINDOW_FLAG_LOGICAL_SIZE),
            Self::Physical { width, height } => (width, height, 0),
        }
    }

    /// What a request asked for: a size of zero on either axis is
    /// [`Size::Any`].
    pub fn of(request: &CreateWindowRequest) -> Self {
        let (width, height) = (request.width, request.height);
        match (width == 0 || height == 0, request.flags & WINDOW_FLAG_LOGICAL_SIZE != 0) {
            (true, _) => Self::Any,
            (false, true) => Self::Logical { width, height },
            (false, false) => Self::Physical { width, height },
        }
    }

    /// In physical pixels at `scale`; `None` for [`Size::Any`].
    pub fn physical(self, scale: Scale) -> Option<(u32, u32)> {
        match self {
            Self::Any => None,
            Self::Logical { width, height } => Some((scale.px_u32(width), scale.px_u32(height))),
            Self::Physical { width, height } => Some((width, height)),
        }
    }
}

pub const MOD_SHIFT: u8 = 1;
pub const MOD_CTRL: u8 = 2;
pub const MOD_ALT: u8 = 4;
pub const MOD_GUI: u8 = 8;
pub const MOD_RELEASED: u8 = 0x10;

toyos::ipc_payload! {
    /// The kernel's [`toyos_abi::input::RawKeyEvent`], as the compositor
    /// forwards it.
    ///
    /// Byte-identical to it and asserted so below: the compositor reads the
    /// keyboard device into an array of these and hands whole ones on, which
    /// is what makes "the compositor translates nothing" a property of the
    /// wire and not of a convention.
    pub struct KeyEvent {
        pub keycode: u8,
        pub modifiers: u8,
    }
}

const _: () = assert!(
    core::mem::size_of::<KeyEvent>() == core::mem::size_of::<toyos_abi::input::RawKeyEvent>(),
);

impl KeyEvent {
    pub const EMPTY: Self = Self { keycode: 0, modifiers: 0 };

    pub fn pressed(&self) -> bool { self.modifiers & MOD_RELEASED == 0 }
    pub fn released(&self) -> bool { self.modifiers & MOD_RELEASED != 0 }
    pub fn shift(&self) -> bool { self.modifiers & MOD_SHIFT != 0 }
    pub fn ctrl(&self) -> bool { self.modifiers & MOD_CTRL != 0 }
    pub fn alt(&self) -> bool { self.modifiers & MOD_ALT != 0 }
    pub fn gui(&self) -> bool { self.modifiers & MOD_GUI != 0 }

    pub fn mods(&self) -> Mods {
        Mods { shift: self.shift(), ctrl: self.ctrl(), alt: self.alt() }
    }
}

/// The same two bytes, for a window that hosts surfaces of its own and has to
/// pass a transition further down.
impl From<KeyEvent> for toyos_abi::input::RawKeyEvent {
    fn from(key: KeyEvent) -> Self {
        Self { keycode: key.keycode, modifiers: key.modifiers }
    }
}

impl From<toyos_abi::input::RawKeyEvent> for KeyEvent {
    fn from(key: toyos_abi::input::RawKeyEvent) -> Self {
        Self { keycode: key.keycode, modifiers: key.modifiers }
    }
}

/// A transition and what it types here, from [`Window::press`].
///
/// Separate from [`KeyEvent`] because they are answers to different questions:
/// [`KeyEvent`] is the wire form, identical on every surface the transition
/// passes through, and this is one surface's reading of it under one layout.
pub struct KeyPress {
    pub keycode: u8,
    pub modifiers: u8,
    text: Emit,
}

impl KeyPress {
    /// What this press types. Empty for a release, a modifier, and every key
    /// the layout leaves undefined.
    pub fn text(&self) -> &str {
        self.text.as_str()
    }

    pub fn pressed(&self) -> bool { self.modifiers & MOD_RELEASED == 0 }
    pub fn released(&self) -> bool { self.modifiers & MOD_RELEASED != 0 }
    pub fn shift(&self) -> bool { self.modifiers & MOD_SHIFT != 0 }
    pub fn ctrl(&self) -> bool { self.modifiers & MOD_CTRL != 0 }
    pub fn alt(&self) -> bool { self.modifiers & MOD_ALT != 0 }
    pub fn gui(&self) -> bool { self.modifiers & MOD_GUI != 0 }
}

pub enum Event {
    KeyInput(KeyEvent),
    MouseInput(MouseEvent),
    ClipboardPaste(Vec<u8>),
    /// The window's size or its scale changed, or both: it has a new buffer.
    Resized,
    Close,
    /// The keyboard layout config changed; this window's translator has
    /// already been re-read. A client that hosts surfaces of its own passes
    /// the same message down to them.
    LayoutChanged,
    Frame,
}

/// The layout named in [`surface::LAYOUT_CONFIG`], or the built-in default
/// when there is no config to read.
///
/// Every translator in the system starts here and comes back here whenever it
/// is told the file moved, which is what stops two surfaces disagreeing about
/// a layout the user only chose once.
pub fn configured_translator() -> Translator {
    let mut translator = Translator::new();
    load_layout(&mut translator);
    translator
}

/// Point `translator` at the configured layout. A config naming a layout that
/// does not exist leaves it where it was — the alternative is silently
/// falling back to `us` on a machine whose owner asked for something else.
pub fn load_layout(translator: &mut Translator) {
    let Ok(config) = std::fs::read_to_string(surface::LAYOUT_CONFIG) else {
        return;
    };
    let name = config.trim();
    if !name.is_empty() && !translator.set_layout(name) {
        eprintln!("window: {:?} names no layout this build has", name);
    }
}

/// Why [`clipboard_set`] put nothing on the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyError {
    /// Longer than [`MAX_CLIPBOARD_BYTES`].
    TooLong(usize),
    /// The exchange with the compositor failed.
    Compositor(CreateError),
}

impl std::fmt::Display for CopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLong(len) => {
                write!(f, "{len} bytes is past the clipboard's {MAX_CLIPBOARD_BYTES}")
            }
            Self::Compositor(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for CopyError {}

impl From<CreateError> for CopyError {
    fn from(e: CreateError) -> Self {
        Self::Compositor(e)
    }
}

/// Put `text` on the system clipboard, over a connection of its own.
///
/// Past [`MAX_INLINE_PAYLOAD`] the text goes into a region the compositor
/// made for it ([`MSG_COPY_BEGIN`]).
pub fn clipboard_set(text: &str) -> Result<(), CopyError> {
    let bytes = text.as_bytes();
    if bytes.len() > MAX_INLINE_PAYLOAD && !copy_fits(bytes.len()) {
        return Err(CopyError::TooLong(bytes.len()));
    }
    Ok(copy(bytes)?)
}

/// [`clipboard_set`]'s exchange, for text the clipboard holds.
fn copy(bytes: &[u8]) -> Result<(), CreateError> {
    let conn = endow::service(COMPOSITOR)?;
    if bytes.len() <= MAX_INLINE_PAYLOAD {
        return Ok(conn.send_bytes(MSG_CLIPBOARD_SET, bytes)?);
    }
    conn.send(MSG_COPY_BEGIN, &ClipboardShmMsg { len: bytes.len() as u32 })?;
    let mut rx = ipc::FrameRx::<0>::new();
    let frame = rx.recv(&conn)?;
    if frame.msg_type != MSG_COPY_REGION {
        return Err(CreateError::Protocol(frame.msg_type));
    }
    let [region] = rx.take_handles::<1>().ok_or(CreateError::NoHandle)?;
    let region = SharedMemory::lent(region, bytes.len()).map_err(CreateError::Lent)?;
    region.bytes().write(0, bytes);
    Ok(conn.signal(MSG_COPY_COMMIT)?)
}

/// The bytes a window buffer of `height` rows of `stride` pixels holds, or
/// `None` for a stride narrower than `width` or a size past the address space:
/// the compositor's word, refused before anything is drawn by it.
fn buffer_bytes(width: u32, stride: u32, height: u32) -> Option<usize> {
    if stride < width {
        return None;
    }
    (stride as usize).checked_mul(height as usize)?.checked_mul(4)
}

pub struct Window {
    conn: Connection,
    /// The compositor's events, each taken whole.
    rx: Box<ipc::FrameRx<MAX_INLINE_PAYLOAD>>,
    poller: Poller,
    /// The buffer, whose one writable handle the compositor sent; the
    /// compositor maps its own read-only.
    shm: Rc<SharedMemory>,
    width: u32,
    height: u32,
    stride: u32,
    pixel_format: u32,
    scale: Scale,
    /// The buffer's generation, which every present carries.
    generation: u32,
    /// This window's own layout state. One per client process, which is what
    /// keeps a dead key typed into one window from composing with a letter
    /// typed into another.
    translator: Translator,
    /// Whether [`Event::Close`] has been handed out. See [`Window::poll_event`].
    closed: bool,
}

impl Window {
    /// Ask the compositor for a window.
    ///
    /// Fails rather than panics: the compositor is entitled to say no — it
    /// bounds what it will hold — and "you may not have a window" is an
    /// ordinary answer to an ordinary request, not a bug in the caller.
    pub fn create(size: Size) -> Result<Self, CreateError> {
        Self::create_with_title(size, "")
    }

    pub fn create_with_title(size: Size, title: &str) -> Result<Self, CreateError> {
        Self::create_with_flags(COMPOSITOR, size, title, 0)
    }

    pub fn create_topmost(size: Size, title: &str) -> Result<Self, CreateError> {
        Self::create_with_flags(COMPOSITOR, size, title, WINDOW_FLAG_TOPMOST)
    }

    /// A window asked for on the compositor's port named `port` in this
    /// program's namespace rather than on `compositor`: which port a window
    /// came in on is what the compositor places it by.
    pub fn create_on(port: &str, size: Size, title: &str) -> Result<Self, CreateError> {
        Self::create_with_flags(port, size, title, 0)
    }

    fn create_with_flags(port: &str, size: Size, title: &str, flags: u8) -> Result<Self, CreateError> {
        let conn = endow::service(port)?;

        let (width, height, unit) = size.wire();
        let mut req = CreateWindowRequest {
            width,
            height,
            flags: flags | unit,
            title_len: 0,
            title: [0; 30],
        };
        let bytes = title.as_bytes();
        let len = bytes.len().min(30);
        req.title[..len].copy_from_slice(&bytes[..len]);
        req.title_len = len as u8;
        conn.send(MSG_CREATE_WINDOW, &req)?;

        // The two answers carry different structs, and a payload of another
        // length than its type's is a refusal from `decode_payload`, not a window.
        let mut rx = ipc::FrameRx::<{ core::mem::size_of::<WindowInfo>() }>::new();
        let frame = rx.recv(&conn)?;
        let payload = rx.payload(frame.payload_len);
        match frame.msg_type {
            MSG_WINDOW_CREATED => {}
            MSG_WINDOW_REFUSED => {
                let refused: WindowRefused = ipc::decode_payload(payload)?;
                return Err(CreateError::from_wire(refused.reason));
            }
            other => return Err(CreateError::Protocol(other)),
        }
        let info: WindowInfo = ipc::decode_payload(payload)?;
        let scale = Scale::from_wire(info.scale).ok_or(CreateError::Protocol(MSG_WINDOW_CREATED))?;
        let Some(buf_size) = buffer_bytes(info.width, info.stride, info.height) else {
            return Err(CreateError::Protocol(MSG_WINDOW_CREATED));
        };
        // The buffer travels in the frame. A compositor that announced a
        // window and sent nothing with it is not serving this client, whatever
        // else it is doing.
        let [buffer] = rx.take_handles::<1>().ok_or(CreateError::NoHandle)?;
        let shm = SharedMemory::lent(buffer, buf_size).map_err(CreateError::Lent)?;

        let poller = Poller::new(1);
        Ok(Self {
            conn,
            rx: Box::new(ipc::FrameRx::new()),
            poller,
            shm: Rc::new(shm),
            width: info.width,
            height: info.height,
            stride: info.stride,
            pixel_format: info.pixel_format,
            scale,
            generation: info.generation,
            translator: configured_translator(),
            closed: false,
        })
    }

    /// Which layout this window is translating with.
    pub fn layout(&self) -> &'static str {
        self.translator.layout()
    }

    /// `key` with what it types on this window under the layout in force.
    ///
    /// **Called once per press by whoever wants the characters, and not at
    /// all by anyone who does not.** That is not a convenience: it advances
    /// the dead-key state, so a surface that is lending its keys to a child —
    /// a terminal while the layout wizard has them — must not call it, or a
    /// `^` the wizard consumed would still be waiting for a base character
    /// when the wizard exits.
    pub fn press(&mut self, key: KeyEvent) -> KeyPress {
        let text =
            if key.pressed() { self.translator.press(key.keycode, key.mods()) } else { Emit::EMPTY };
        KeyPress { keycode: key.keycode, modifiers: key.modifiers, text }
    }

    /// Tell the compositor that [`surface::LAYOUT_CONFIG`] changed, so every
    /// other window re-reads it too.
    pub fn notify_layout_changed(&self) {
        let _ = self.conn.signal(MSG_LAYOUT_CHANGED);
    }

    pub fn recv_event(&mut self) -> Event {
        let Ok(frame) = self.rx.recv(&self.conn) else {
            return Event::Close;
        };
        self.decode_event(frame)
    }

    /// The next event, or `None` when `timeout_nanos` passes — and `None`
    /// forever once [`Event::Close`] has been handed out.
    ///
    /// **`Close` is the last element of this stream, and it is delivered
    /// exactly once.** Without the latch a closed window is an infinite source
    /// of it: the compositor drops the connection, the handle is then permanently
    /// read-ready at EOF, and every call returns `Some(Event::Close)` again.
    /// A caller that drains until `None` — which is the ordinary shape, and is
    /// what `winit`'s ToyOS backend does — never leaves that loop, so closing
    /// the window spun the client on a core instead of ending it (snake, on
    /// the T14). `compositor_client_death`'s fifth case is the gate: it
    /// destroys its own window and requires the poll after `Close` to answer
    /// `None`.
    pub fn poll_event(&mut self, timeout_nanos: u64) -> Option<Event> {
        if self.closed {
            return None;
        }
        self.poller.watch(&self.conn, READABLE, 0);
        let mut ready = false;
        self.poller.wait(1, timeout_nanos, |_| ready = true);
        if !ready {
            return None;
        }
        let event = self.recv_event();
        self.closed = matches!(&event, Event::Close);
        Some(event)
    }

    /// A message the compositor cannot have meant closes the window rather
    /// than killing the client: this side is a library inside somebody else's
    /// program, and it has a way to say "the session is over".
    fn decode_event(&mut self, frame: ipc::Frame) -> Event {
        let payload = self.rx.payload(frame.payload_len);
        match frame.msg_type {
            MSG_KEY_INPUT => match ipc::decode_payload::<KeyEvent>(payload) {
                Ok(key) => Event::KeyInput(key),
                Err(_) => Event::Close,
            },
            MSG_LAYOUT_CHANGED => {
                load_layout(&mut self.translator);
                Event::LayoutChanged
            }
            MSG_MOUSE_INPUT => match ipc::decode_payload(payload) {
                Ok(ev) => Event::MouseInput(ev),
                Err(_) => Event::Close,
            },
            MSG_WINDOW_RESIZED => {
                let Ok(info) = ipc::decode_payload::<ResizeInfo>(payload) else {
                    return Event::Close;
                };
                // A scale outside the protocol is a compositor this client
                // cannot draw for.
                let Some(scale) = Scale::from_wire(info.scale) else {
                    return Event::Close;
                };
                let Some(buf_size) = buffer_bytes(info.width, info.stride, info.height) else {
                    return Event::Close;
                };
                // The compositor shows the old buffer only until this
                // window's next present, which it reads from the new one, so a
                // window that cannot map the new one has nothing to present.
                let Some([buffer]) = self.rx.take_handles::<1>() else {
                    return Event::Close;
                };
                let Ok(shm) = SharedMemory::lent(buffer, buf_size) else {
                    return Event::Close;
                };
                self.shm = Rc::new(shm);
                self.width = info.width;
                self.height = info.height;
                self.stride = info.stride;
                self.pixel_format = info.pixel_format;
                self.scale = scale;
                self.generation = info.generation;
                Event::Resized
            }
            MSG_CLIPBOARD_PASTE => Event::ClipboardPaste(payload.to_vec()),
            MSG_CLIPBOARD_PASTE_SHM => {
                let Ok(info) = ipc::decode_payload::<ClipboardShmMsg>(payload) else {
                    return Event::Close;
                };
                let Some([buffer]) = self.rx.take_handles::<1>() else {
                    return Event::ClipboardPaste(Vec::new());
                };
                let Ok(shm) = ReadOnly::lent(buffer, info.len as usize) else {
                    return Event::ClipboardPaste(Vec::new());
                };
                Event::ClipboardPaste(shm.bytes().to_vec())
            }
            MSG_WINDOW_CLOSE => Event::Close,
            MSG_FRAME => Event::Frame,
            _ => Event::Close,
        }
    }

    pub fn set_cursor(&self, style: u8) {
        let _ = self.conn.send(MSG_SET_CURSOR, &(style as u32));
    }

    /// Hand over the whole window.
    ///
    /// For a client that draws without keeping track of where — the honest
    /// answer for one of those is "all of it", and a damage rect it made up
    /// would cost the compositor correctness rather than time.
    pub fn present(&self) {
        self.present_damage(Rect { x: 0, y: 0, w: self.width, h: self.height });
    }

    /// Hand over `damage` and nothing else.
    ///
    /// The rect is in window pixels. Pixels outside it are the compositor's to
    /// leave alone, so a client that names less than it changed leaves a stale
    /// screen — [`Screen::take_damage`] is where the rect comes from for a
    /// client that composes through one.
    pub fn present_damage(&self, damage: Rect) {
        let Rect { x, y, w, h } = damage;
        let _ = self.conn.send(MSG_PRESENT, &PresentRect { x, y, w, h, generation: self.generation });
    }

    pub fn handle(&self) -> toyos::BorrowedHandle<'_> {
        self.conn.as_handle()
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The scale the compositor shows this window at: its buffer is
    /// [`width`](Self::width) by [`height`](Self::height) physical pixels,
    /// drawn at this many per logical one.
    pub fn scale(&self) -> Scale {
        self.scale
    }

    /// [`scale`](Self::scale) as a factor.
    pub fn scale_factor(&self) -> f64 {
        self.scale.factor()
    }

    /// The buffer, drawn with plain stores. One made before a resize draws
    /// into the buffer it replaced, which nothing shows.
    pub fn framebuffer(&self) -> Framebuffer<'static> {
        // SAFETY: the compositor maps the buffer read-only and sent this
        // process its one writable handle, which this window owns and lends
        // to nothing, so nothing duplicates or sends it; every value over
        // `self.shm` is on this thread, as `Rc` keeps them.
        unsafe {
            Framebuffer::mapped(
                Rc::clone(&self.shm),
                self.width as usize,
                self.height as usize,
                self.stride as usize,
                self.pixel_format,
            )
        }
    }

    /// The same buffer as [`Window::framebuffer`], for a client that composes
    /// elsewhere and only ever hands finished pixels over.
    pub fn screen(&self) -> Screen {
        Screen::new(
            Rc::clone(&self.shm),
            self.width as usize,
            self.height as usize,
            self.stride as usize,
            self.pixel_format,
        )
    }
}

impl std::fmt::Debug for Window {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Window")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("scale", &self.scale)
            .finish_non_exhaustive()
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = self.conn.signal(MSG_DESTROY_WINDOW);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(size: Size) -> CreateWindowRequest {
        let (width, height, unit) = size.wire();
        CreateWindowRequest { width, height, flags: WINDOW_FLAG_TOPMOST | unit, title_len: 0, title: [0; 30] }
    }

    #[test]
    fn a_requested_size_says_which_pixels_it_is_in() {
        for size in [
            Size::Any,
            Size::Logical { width: 200, height: 100 },
            Size::Physical { width: 200, height: 100 },
        ] {
            assert_eq!(Size::of(&request(size)), size);
        }
        let two: Scale = "2".parse().unwrap();
        assert_eq!(Size::Logical { width: 200, height: 100 }.physical(two), Some((400, 200)));
        assert_eq!(Size::Physical { width: 200, height: 100 }.physical(two), Some((200, 100)));
        assert_eq!(Size::Any.physical(two), None);
    }

    #[test]
    fn a_buffer_narrower_than_its_window_is_refused_and_one_that_fits_is_counted() {
        assert_eq!(buffer_bytes(10, 10, 3), Some(120));
        assert_eq!(buffer_bytes(10, 12, 3), Some(144));
        assert_eq!(buffer_bytes(10, 9, 3), None);
        assert_eq!(buffer_bytes(u32::MAX, u32::MAX, u32::MAX), None, "past the address space");
    }

    /// A zero on either axis asks the compositor to choose, as it always has.
    #[test]
    fn a_zero_extent_is_any_size() {
        let half = CreateWindowRequest { width: 300, height: 0, flags: 0, title_len: 0, title: [0; 30] };
        assert_eq!(Size::of(&half), Size::Any);
    }
}
