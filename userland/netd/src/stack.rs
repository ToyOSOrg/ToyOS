//! netd's bindings to Netstack3's core: everything the core asks of the
//! program it runs in.
//!
//! **One thread drives the core**, netd's loop, so every lock the core takes
//! is uncontended and every callback below runs inside a call netd made. The
//! core owns the protocol; this module owns the clock, the timers, the random
//! source, the frames the core makes, and the buffers its sockets fill:
//!
//! - **The clock** is the kernel's monotonic clock, counted from this
//!   process's start ([`StackTime`]).
//! - **Timers** are a heap netd's loop fires ([`Timers`]); a fired timer is
//!   handed back to the core with the unique id it was created with.
//! - **Randomness** is the kernel's random source, asked on every draw
//!   ([`KernelRng`]): the core's initial sequence numbers (RFC 6528), its
//!   ephemeral ports (RFC 6056) and its opaque identifiers all rest on it.
//! - **Frames out** join [`crate::egress`]; netd's loop hands them to the
//!   ring.
//! - **TCP** keeps each connection's received bytes in a ring netd shares
//!   with the core ([`Received`]), because the core drops its end when the
//!   peer's FIN arrives and the client may not have read them yet; the send
//!   ring is the core's own ([`SendRing`]), filled through `with_send_buffer`.
//! - **UDP** keeps each socket's received datagrams in its own bounded queue
//!   ([`Inbox`]), the socket's external data.
//!
//! IPv4 is the one protocol netd serves; the IPv6 half of every trait is
//! implemented because the core's bindings trait asks for both, and the device
//! runs with IPv6 disabled.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::convert::Infallible;
use std::fmt::{self, Debug, Display};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use net_types::ethernet::Mac;
use net_types::ip::{Ip, IpVersion};
use net_types::UnicastAddr;
use netstack3_core::device::{
    DeviceClassMatcher, DeviceId, DeviceIdAndNameMatcher, DeviceLayerEventDispatcher,
    DeviceLayerStateTypes, DeviceSendFrameError, EthernetDeviceEvent, EthernetDeviceId,
    EthernetLinkDevice, TxBufferAllocator,
    LoopbackDeviceId, PureIpDeviceId, ReceiveQueueBindingsContext, TransmitQueueBindingsContext,
    WeakDeviceId,
};
use netstack3_core::device_socket::{
    DeviceSocketBindingsContext, DeviceSocketTypes, Frame, ReceiveFrameError, SocketId,
};
use netstack3_core::filter::{
    FilterIpExt, FilterIpPacket, SocketEgressFilterResult, SocketIngressFilterResult,
    SocketOpsFilter, SocketOpsFilterBindingContext,
};
use netstack3_core::icmp::{IcmpEchoBindingsContext, IcmpEchoBindingsTypes, IcmpSocketId, ReceiveIcmpEchoError};
use netstack3_core::inspect::{InspectableValue, Inspector};
use netstack3_core::ip::{
    IpDeviceEvent, IpLayerEvent, MarkDomain, MarksBindingsContext, RawIpSocketId,
    RawIpSocketsBindingsContext, RawIpSocketsBindingsTypes, ReceivePacketError,
    RouterAdvertisementEvent,
};
use netstack3_core::neighbor::{LinkResolutionContext, LinkResolutionNotifier};
use netstack3_core::routes::Marks;
use netstack3_core::filter::SocketInfo as OpsSocketInfo;
use netstack3_core::sync::{DynDebugReferences, RcNotifier};
use netstack3_core::tcp::{
    Buffer, BufferLimits, BufferSizes, FragmentedPayload, IntoBuffers, ListenerNotifier, Payload,
    ReceiveBuffer, SendBuffer, TcpBindingsTypes, TcpSettings, TcpSocketDestructionContext,
    TcpSocketDiagnostics,
};
use netstack3_core::types::BufferSizeSettings;
use netstack3_core::udp::{
    ReceiveUdpError, UdpBindingsTypes, UdpPacketMeta, UdpReceiveBindingsContext, UdpSocketId,
};
use netstack3_core::{
    CoreTxMetadata, DeferredResourceRemovalContext, EventContext, InstantBindingsTypes,
    InstantContext, ReferenceNotifiers, RngContext, SettingsContext, SocketDiagnosticsSeed,
    TimerBindingsTypes, TimerContext, TimerId, TxMetadataBindingsTypes,
};
use netstack3_core::sync::RemoveResourceResultWithContext;
use netstack3_core::ip::IpRoutingBindingsTypes;
use packet::{Buf, BufferMut};
use zerocopy::SplitByteSlice;

use crate::egress::Egress;

/// Payload bytes each direction of a TCP connection buffers inside netd, before
/// the window closes and the peer is asked to wait.
pub const TCP_BUFFER: usize = 65536;

/// Datagrams one UDP socket holds before the next is dropped, as a full socket
/// buffer drops on any stack.
pub const UDP_DATAGRAMS: usize = 16;

// --- The clock ---------------------------------------------------------------

/// The one clock the core reads: the kernel's monotonic time since this
/// process began.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct StackTime(Duration);

impl StackTime {
    /// The time since `epoch`.
    pub fn since(epoch: std::time::Instant) -> Self {
        Self(epoch.elapsed())
    }

    /// The same moment as a `std` instant, `epoch` being this clock's zero.
    pub fn at(self, epoch: std::time::Instant) -> std::time::Instant {
        epoch + self.0
    }
}

impl netstack3_core::Instant for StackTime {
    fn checked_duration_since(&self, earlier: Self) -> Option<Duration> {
        self.0.checked_sub(earlier.0)
    }

    fn checked_add(&self, duration: Duration) -> Option<Self> {
        self.0.checked_add(duration).map(Self)
    }

    fn saturating_add(&self, duration: Duration) -> Self {
        Self(self.0.saturating_add(duration))
    }

    fn checked_sub(&self, duration: Duration) -> Option<Self> {
        self.0.checked_sub(duration).map(Self)
    }
}

impl InspectableValue for StackTime {
    fn record<I: Inspector>(&self, name: &str, inspector: &mut I) {
        inspector.record_uint(name, self.0.as_nanos() as u64)
    }
}

/// [`StackTime`] as one atomic word of nanoseconds.
#[derive(Debug)]
pub struct AtomicStackTime(AtomicU64);

fn nanos(t: StackTime) -> u64 {
    u64::try_from(t.0.as_nanos()).expect("netd: 584 years of uptime")
}

impl netstack3_core::AtomicInstant<StackTime> for AtomicStackTime {
    fn new(instant: StackTime) -> Self {
        Self(AtomicU64::new(nanos(instant)))
    }

    fn load(&self, ordering: Ordering) -> StackTime {
        StackTime(Duration::from_nanos(self.0.load(ordering)))
    }

    fn store(&self, instant: StackTime, ordering: Ordering) {
        self.0.store(nanos(instant), ordering)
    }

    fn store_max(&self, instant: StackTime, ordering: Ordering) {
        self.0.fetch_max(nanos(instant), ordering);
    }
}

// --- Randomness --------------------------------------------------------------

/// The kernel's random source, asked on every draw.
///
/// **A refusal ends netd**: a sequence number or a port anyone can predict is
/// a forged segment's way in, and there is no second source to fall back on.
pub struct KernelRng;

impl rand::TryRng for KernelRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        let mut bytes = [0u8; 4];
        self.try_fill_bytes(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        let mut bytes = [0u8; 8];
        self.try_fill_bytes(&mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Infallible> {
        toyos_abi::syscall::random(dest)
            .unwrap_or_else(|e| panic!("netd: the kernel's random source refused the stack: {e:?}"));
        Ok(())
    }
}

impl rand::TryCryptoRng for KernelRng {}

// --- Timers ------------------------------------------------------------------

/// A timer the core created: which dispatch it fires, under which id.
#[derive(Debug)]
pub struct Timer {
    id: u64,
    dispatch: TimerId<Bindings>,
}

/// Every scheduled timer, soonest first. A timer that is rescheduled or
/// cancelled leaves the heap at once, so nothing fires that the core no longer
/// asked for.
#[derive(Default)]
pub struct Timers {
    next_id: u64,
    heap: BTreeSet<(StackTime, u64)>,
    scheduled: HashMap<u64, (StackTime, TimerId<Bindings>)>,
}

impl Timers {
    fn unschedule(&mut self, id: u64) -> Option<StackTime> {
        let (at, _) = self.scheduled.remove(&id)?;
        assert!(self.heap.remove(&(at, id)), "netd: a scheduled timer missing from the heap");
        Some(at)
    }

    /// The soonest scheduled timer's instant.
    pub fn next(&self) -> Option<StackTime> {
        self.heap.first().map(|&(at, _)| at)
    }

    /// Take the soonest timer due at `now`, if one is.
    pub fn pop_due(&mut self, now: StackTime) -> Option<(TimerId<Bindings>, u64)> {
        let &(at, id) = self.heap.first()?;
        if at > now {
            return None;
        }
        self.heap.remove(&(at, id));
        let (_, dispatch) = self.scheduled.remove(&id).expect("netd: a heap entry with no timer");
        Some((dispatch, id))
    }
}

// --- UDP ---------------------------------------------------------------------

/// A datagram a UDP socket received, and whom from.
pub struct Datagram {
    pub from: [u8; 4],
    pub port: u16,
    pub bytes: Vec<u8>,
}

/// One UDP socket's received datagrams, oldest first, at most
/// [`UDP_DATAGRAMS`].
#[derive(Default)]
pub struct Inbox(Mutex<VecDeque<Datagram>>);

impl Debug for Inbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inbox").finish_non_exhaustive()
    }
}

impl Inbox {
    /// Take the oldest datagram.
    pub fn take(&self) -> Option<Datagram> {
        self.0.lock().expect("netd: an inbox's lock").pop_front()
    }
}

// --- TCP ---------------------------------------------------------------------

/// A ring of bytes: readable from `head`, `len` of them.
struct Ring {
    storage: Vec<u8>,
    head: usize,
    len: usize,
}

impl Ring {
    fn new(capacity: usize) -> Self {
        Self { storage: vec![0; capacity], head: 0, len: 0 }
    }

    fn capacity(&self) -> usize {
        self.storage.len()
    }

    /// The readable bytes from `offset` on, as at most two slices.
    fn readable(&self, offset: usize) -> [&[u8]; 2] {
        assert!(offset <= self.len, "netd: a ring read past its readable bytes");
        let start = (self.head + offset) % self.capacity().max(1);
        let len = self.len - offset;
        let first = len.min(self.capacity() - start);
        [&self.storage[start..start + first], &self.storage[..len - first]]
    }

    /// Mark `count` readable bytes read.
    fn consume(&mut self, count: usize) {
        assert!(count <= self.len, "netd: a ring consumed past its readable bytes");
        self.len -= count;
        self.head = (self.head + count) % self.capacity().max(1);
    }

    /// Write `data` at `offset` past the readable bytes, as far as the free
    /// space reaches, answering how much was written.
    fn write_at<P: Payload>(&mut self, offset: usize, data: &P) -> usize {
        let free = self.capacity() - self.len;
        if offset >= free {
            return 0;
        }
        let n = data.len().min(free - offset);
        let start = (self.head + self.len + offset) % self.capacity();
        let first = n.min(self.capacity() - start);
        data.partial_copy(0, &mut self.storage[start..start + first]);
        data.partial_copy(first, &mut self.storage[..n - first]);
        n
    }

    /// Append as much of `bytes` as fits, answering how much did.
    fn push(&mut self, bytes: &[u8]) -> usize {
        let n = self.write_at(0, &bytes);
        self.len += n;
        n
    }
}

/// What netd and the core share about one TCP socket.
pub struct Shared {
    /// The peer's bytes the core has made readable and netd has not taken.
    received: Mutex<Ring>,
    /// Whether the core still holds its end of `received`. It lets go at the
    /// peer's FIN and when the connection ends; every byte before is in the
    /// ring by then.
    receiving: AtomicBool,
    /// Whether the connection was established: the core converts a connecting
    /// socket's buffers once its handshake completes.
    established: AtomicBool,
    /// For a listener, the connections ready to accept.
    incoming: AtomicUsize,
}

impl Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shared")
            .field("receiving", &self.receiving)
            .field("established", &self.established)
            .field("incoming", &self.incoming)
            .finish_non_exhaustive()
    }
}

impl Shared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            received: Mutex::new(Ring::new(0)),
            receiving: AtomicBool::new(false),
            established: AtomicBool::new(false),
            incoming: AtomicUsize::new(0),
        })
    }

    /// Hand the readable bytes to `take`, which answers how many it took.
    pub fn read(&self, take: impl FnOnce(&[u8]) -> usize) -> usize {
        let mut ring = self.received.lock().expect("netd: a receive ring's lock");
        let [first, _] = ring.readable(0);
        let n = take(first);
        ring.consume(n);
        n
    }

    /// Bytes the peer sent that netd has not taken.
    pub fn unread(&self) -> usize {
        self.received.lock().expect("netd: a receive ring's lock").len
    }

    /// Whether the core still writes into the receive ring.
    pub fn receiving(&self) -> bool {
        self.receiving.load(Ordering::Relaxed)
    }

    pub fn established(&self) -> bool {
        self.established.load(Ordering::Relaxed)
    }

    pub fn incoming(&self) -> usize {
        self.incoming.load(Ordering::Relaxed)
    }
}

/// What netd hands the core with every TCP socket it creates: the buffers it
/// provides if the socket connects, and the listener's notifier if it listens.
#[derive(Debug)]
pub struct SocketExtra(pub Arc<Shared>);

impl SocketExtra {
    pub fn new() -> Self {
        Self(Shared::new())
    }
}

impl IntoBuffers<Received, SendRing> for SocketExtra {
    fn into_buffers(self, sizes: BufferSizes) -> (Received, SendRing) {
        buffers(self.0, sizes)
    }
}

impl ListenerNotifier for SocketExtra {
    fn new_incoming_connections(&mut self, count: usize) {
        self.0.incoming.store(count, Ordering::Relaxed);
    }
}

fn buffers(shared: Arc<Shared>, BufferSizes { send, receive }: BufferSizes) -> (Received, SendRing) {
    *shared.received.lock().expect("netd: a receive ring's lock") = Ring::new(receive);
    shared.receiving.store(true, Ordering::Relaxed);
    shared.established.store(true, Ordering::Relaxed);
    (Received(shared), SendRing(Ring::new(send)))
}

/// The core's end of a connection's receive ring.
#[derive(Debug)]
pub struct Received(Arc<Shared>);

impl Drop for Received {
    fn drop(&mut self) {
        self.0.receiving.store(false, Ordering::Relaxed);
    }
}

impl Buffer for Received {
    fn limits(&self) -> BufferLimits {
        let ring = self.0.received.lock().expect("netd: a receive ring's lock");
        BufferLimits { capacity: ring.capacity(), len: ring.len }
    }

    fn target_capacity(&self) -> usize {
        self.limits().capacity
    }

    fn request_capacity(&mut self, size: usize) {
        unimplemented!("netd: its receive rings are fixed at {TCP_BUFFER} bytes, and {size} was asked")
    }
}

impl ReceiveBuffer for Received {
    fn write_at<P: Payload>(&mut self, offset: usize, data: &P) -> usize {
        self.0.received.lock().expect("netd: a receive ring's lock").write_at(offset, data)
    }

    fn make_readable(&mut self, count: usize, _has_outstanding: bool) {
        let mut ring = self.0.received.lock().expect("netd: a receive ring's lock");
        assert!(ring.len + count <= ring.capacity(), "netd: the core made unwritten bytes readable");
        ring.len += count;
    }
}

/// A connection's send ring, the core's own: netd appends the client's bytes
/// through `with_send_buffer`, and the core reads and releases them.
pub struct SendRing(Ring);

impl Debug for SendRing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendRing").field("len", &self.0.len).finish_non_exhaustive()
    }
}

impl SendRing {
    /// Append as much of `bytes` as fits, answering how much did.
    pub fn push(&mut self, bytes: &[u8]) -> usize {
        self.0.push(bytes)
    }

    /// Free space.
    pub fn room(&self) -> usize {
        self.0.capacity() - self.0.len
    }
}

impl Buffer for SendRing {
    fn limits(&self) -> BufferLimits {
        BufferLimits { capacity: self.0.capacity(), len: self.0.len }
    }

    fn target_capacity(&self) -> usize {
        self.0.capacity()
    }

    fn request_capacity(&mut self, size: usize) {
        unimplemented!("netd: its send rings are fixed at {TCP_BUFFER} bytes, and {size} was asked")
    }
}

impl SendBuffer for SendRing {
    type Payload<'a> = FragmentedPayload<'a, 2>;

    fn mark_read(&mut self, count: usize) {
        self.0.consume(count)
    }

    fn peek_with<'a, F, R>(&'a mut self, offset: usize, f: F) -> R
    where
        F: FnOnce(Self::Payload<'a>) -> R,
    {
        f(FragmentedPayload::new(self.0.readable(offset)))
    }
}

// --- Uninhabited -------------------------------------------------------------

/// A kind netd never makes: a custom packet matcher.
#[derive(Clone, Debug)]
pub enum Never {}

impl InspectableValue for Never {
    fn record<I: Inspector>(&self, _name: &str, _inspector: &mut I) {
        match *self {}
    }
}

// --- Deferred removal --------------------------------------------------------

/// A resource the core could not release at once, and where it arrives once
/// its last reference goes.
pub struct Slot<T>(Arc<Mutex<Option<T>>>);

impl<T> Debug for Slot<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Slot").finish_non_exhaustive()
    }
}

impl<T: Send> RcNotifier<T> for Slot<T> {
    fn notify(&mut self, data: T) {
        *self.0.lock().expect("netd: a removal slot's lock") = Some(data);
    }
}

/// A removal the core deferred, dropped once it has arrived.
trait Pending: Send {
    fn arrived(&self) -> bool;
}

impl<T: Send> Pending for Slot<T> {
    fn arrived(&self) -> bool {
        self.0.lock().expect("netd: a removal slot's lock").is_some()
    }
}

// --- The bindings ------------------------------------------------------------

/// The device's state as the core keeps it for netd: nothing.
#[derive(Default)]
pub struct DeviceState;

impl DeviceClassMatcher<()> for DeviceState {
    fn device_class_matches(&self, _device_class: &()) -> bool {
        true
    }
}

/// The one name netd's one device has.
#[derive(Debug)]
pub struct DeviceName;

impl Display for DeviceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("eth0")
    }
}

impl DeviceIdAndNameMatcher for DeviceName {
    fn id_matches(&self, _id: &std::num::NonZeroU64) -> bool {
        unimplemented!("netd installs no interface matcher")
    }

    fn name_matches(&self, _name: &str) -> bool {
        unimplemented!("netd installs no interface matcher")
    }
}

/// Everything the core asks of netd.
pub struct Bindings {
    epoch: std::time::Instant,
    pub timers: Timers,
    /// The frames the core made, waiting for the ring.
    pub egress: Egress,
    settings: TcpSettings,
    /// Removals the core deferred until their last reference goes.
    deferred: Vec<Box<dyn Pending>>,
    /// Multicast groups the device joined, for the driver's filter.
    pub joined: Vec<[u8; 6]>,
}

impl Bindings {
    pub fn new(epoch: std::time::Instant) -> Self {
        let size = NonZeroUsize::new(TCP_BUFFER).expect("a TCP buffer holds bytes");
        let sizes = BufferSizeSettings::new(size, size, size).expect("min <= default <= max");
        Self {
            epoch,
            timers: Timers::default(),
            egress: Egress::default(),
            settings: TcpSettings { receive_buffer: sizes, send_buffer: sizes },
            deferred: Vec::new(),
            joined: Vec::new(),
        }
    }

    /// The moment the core would call now.
    pub fn now(&self) -> StackTime {
        StackTime::since(self.epoch)
    }

    /// Where this clock's zero is.
    pub fn epoch(&self) -> std::time::Instant {
        self.epoch
    }

    /// Drop every deferred removal whose value has arrived.
    pub fn sweep_deferred(&mut self) {
        self.deferred.retain(|pending| !pending.arrived());
    }

    /// Removals still waiting for their last reference.
    pub fn deferred(&self) -> usize {
        self.deferred.len()
    }
}

impl InstantBindingsTypes for Bindings {
    type Instant = StackTime;
    type AtomicInstant = AtomicStackTime;
}

impl InstantContext for Bindings {
    fn now(&self) -> StackTime {
        Bindings::now(self)
    }
}

impl TimerBindingsTypes for Bindings {
    type Timer = Timer;
    type DispatchId = TimerId<Self>;
    type UniqueTimerId = u64;
}

impl TimerContext for Bindings {
    fn new_timer(&mut self, dispatch: TimerId<Self>) -> Timer {
        let id = self.timers.next_id;
        self.timers.next_id += 1;
        Timer { id, dispatch }
    }

    fn schedule_timer_instant(&mut self, time: StackTime, timer: &mut Timer) -> Option<StackTime> {
        let before = self.timers.unschedule(timer.id);
        self.timers.heap.insert((time, timer.id));
        self.timers.scheduled.insert(timer.id, (time, timer.dispatch.clone()));
        before
    }

    fn cancel_timer(&mut self, timer: &mut Timer) -> Option<StackTime> {
        self.timers.unschedule(timer.id)
    }

    fn scheduled_instant(&self, timer: &mut Timer) -> Option<StackTime> {
        self.timers.scheduled.get(&timer.id).map(|&(at, _)| at)
    }

    fn unique_timer_id(&self, timer: &Timer) -> u64 {
        timer.id
    }
}

impl RngContext for Bindings {
    type Rng<'a> = KernelRng;

    fn rng(&mut self) -> KernelRng {
        KernelRng
    }
}

impl TxMetadataBindingsTypes for Bindings {
    type TxMetadata = CoreTxMetadata<Self>;
}

impl netstack3_core::MatcherBindingsTypes for Bindings {
    type DeviceClass = ();
    type BindingsPacketMatcher = Never;
}

impl netstack3_core::device::DeviceBufferBindingsTypes for Bindings {
    type TxBuffer = Buf<Vec<u8>>;
    type TxAllocator = VecAllocator;
}

impl IpRoutingBindingsTypes for Bindings {
    type RoutingTableId = ();
}

impl MarksBindingsContext for Bindings {
    fn marks_to_keep_on_egress() -> &'static [MarkDomain] {
        &[]
    }

    fn marks_to_set_on_ingress() -> &'static [MarkDomain] {
        &[]
    }
}

impl SettingsContext<TcpSettings> for Bindings {
    fn settings(&self) -> impl std::ops::Deref<Target = TcpSettings> + '_ {
        &self.settings
    }
}

/// Every socket passes: netd filters nothing.
struct PassAll;

impl SocketOpsFilter<DeviceId<Bindings>> for PassAll {
    fn on_egress<I: FilterIpExt, P: FilterIpPacket<I>>(
        &self,
        _packet: &P,
        _device: &DeviceId<Bindings>,
        _socket_info: OpsSocketInfo,
        _marks: &Marks,
    ) -> SocketEgressFilterResult {
        SocketEgressFilterResult::Pass { congestion: false }
    }

    fn on_ingress(
        &self,
        _ip_version: IpVersion,
        _packet: packet::FragmentedByteSlice<'_, &[u8]>,
        _header_len: usize,
        _device: &DeviceId<Bindings>,
        _socket_info: OpsSocketInfo,
        _marks: &Marks,
    ) -> SocketIngressFilterResult {
        SocketIngressFilterResult::Accept
    }
}

impl SocketOpsFilterBindingContext<DeviceId<Bindings>> for Bindings {
    fn socket_ops_filter(&self) -> impl SocketOpsFilter<DeviceId<Bindings>> {
        PassAll
    }
}

impl TcpBindingsTypes for Bindings {
    type ReceiveBuffer = Received;
    type SendBuffer = SendRing;
    type ReturnedBuffers = Arc<Shared>;
    type ListenerNotifierOrProvidedBuffers = SocketExtra;

    fn new_passive_open_buffers(sizes: BufferSizes) -> (Received, SendRing, Arc<Shared>) {
        let shared = Shared::new();
        let (received, send) = buffers(Arc::clone(&shared), sizes);
        (received, send, shared)
    }
}

impl TcpSocketDestructionContext for Bindings {
    fn defer_tcp_socket_destruction<I, S>(&self, _result: RemoveResourceResultWithContext<S, Self>)
    where
        I: Ip,
        S: SocketDiagnosticsSeed<Output = TcpSocketDiagnostics<I, StackTime>> + Send + 'static,
    {
        // netd reads no diagnostics of a destroyed socket.
    }
}

impl UdpBindingsTypes for Bindings {
    type ExternalData<I: Ip> = Inbox;
    type SendToken = ();
}

impl<I: netstack3_core::IpExt> UdpReceiveBindingsContext<I, DeviceId<Self>> for Bindings {
    fn receive_udp(
        &mut self,
        id: &UdpSocketId<I, WeakDeviceId<Self>, Self>,
        _device_id: &DeviceId<Self>,
        meta: UdpPacketMeta<I>,
        body: &[u8],
    ) -> Result<(), ReceiveUdpError> {
        let mut queue = id.external_data().0.lock().expect("netd: an inbox's lock");
        if queue.len() >= UDP_DATAGRAMS {
            return Err(ReceiveUdpError::QueueFull);
        }
        let from = I::map_ip_in(meta.src_ip, |v4| v4.ipv4_bytes(), |_v6| unreachable!("netd serves IPv4 alone"));
        queue.push_back(Datagram { from, port: meta.src_port.map_or(0, |p| p.get()), bytes: body.to_vec() });
        Ok(())
    }

    fn on_socket_error(
        &mut self,
        _id: &UdpSocketId<I, WeakDeviceId<Self>, Self>,
        _err: netstack3_core::socket::PendingDatagramSocketError,
    ) {
        // An ICMP error for a datagram netd sent: a lookup's own wait covers a
        // server that does not answer, and no other UDP client reads errors.
    }
}

impl IcmpEchoBindingsTypes for Bindings {
    type ExternalData<I: Ip> = ();
    type SendToken = ();
}

impl<I: netstack3_core::IpExt> IcmpEchoBindingsContext<I, DeviceId<Self>> for Bindings {
    fn receive_icmp_echo_reply<B: BufferMut>(
        &mut self,
        _conn: &IcmpSocketId<I, WeakDeviceId<Self>, Self>,
        _device: &DeviceId<Self>,
        _src_ip: I::Addr,
        _dst_ip: I::Addr,
        _id: u16,
        _data: B,
    ) -> Result<(), ReceiveIcmpEchoError> {
        unreachable!("netd opens no ICMP echo sockets")
    }
}

impl RawIpSocketsBindingsTypes for Bindings {
    type RawIpSocketState<I: Ip> = ();
}

impl<I: netstack3_core::IpExt> RawIpSocketsBindingsContext<I, DeviceId<Self>> for Bindings {
    fn receive_packet<B: SplitByteSlice>(
        &self,
        _socket: &RawIpSocketId<I, WeakDeviceId<Self>, Self>,
        _packet: &I::Packet<B>,
        _device: &DeviceId<Self>,
    ) -> Result<(), ReceivePacketError> {
        unreachable!("netd opens no raw IP sockets")
    }
}

impl DeviceSocketTypes for Bindings {
    type SocketState<D: Send + Sync + Debug> = ();
}

impl DeviceSocketBindingsContext<DeviceId<Self>> for Bindings {
    fn receive_frame(
        &self,
        _socket_id: &SocketId<Self>,
        _device: &DeviceId<Self>,
        _frame: Frame<&[u8]>,
        _raw_frame: &[u8],
    ) -> Result<(), ReceiveFrameError> {
        unreachable!("netd opens no device sockets")
    }
}

impl DeviceLayerStateTypes for Bindings {
    type LoopbackDeviceState = DeviceState;
    type EthernetDeviceState = DeviceState;
    type PureIpDeviceState = DeviceState;
    type BlackholeDeviceState = DeviceState;
    type DeviceIdentifier = DeviceName;
}

impl ReceiveQueueBindingsContext<LoopbackDeviceId<Self>> for Bindings {
    fn wake_rx_task(&mut self, _device: &LoopbackDeviceId<Self>) {
        unreachable!("netd installs no loopback device")
    }
}

impl<D> TransmitQueueBindingsContext<D> for Bindings {
    fn wake_tx_task(&mut self, _device: &D) {
        unreachable!("netd's device has no transmit queue in the core: every frame is sent at once")
    }
}

impl DeviceLayerEventDispatcher for Bindings {
    type DequeueContext = Infallible;

    fn send_ethernet_frame(
        &mut self,
        _device: &EthernetDeviceId<Self>,
        frame: Buf<Vec<u8>>,
        _dequeue_context: Option<&mut Infallible>,
        _csum_offload: Option<netstack3_core::ChecksumOffloadResult>,
    ) -> Result<(), DeviceSendFrameError> {
        self.egress.push(frame.into_inner());
        Ok(())
    }

    fn send_ip_packet(
        &mut self,
        _device: &PureIpDeviceId<Self>,
        _packet: Buf<Vec<u8>>,
        _ip_version: IpVersion,
        _dequeue_context: Option<&mut Infallible>,
        _csum_offload: Option<netstack3_core::ChecksumOffloadResult>,
    ) -> Result<(), DeviceSendFrameError> {
        unreachable!("netd installs no pure IP device")
    }
}

impl ReferenceNotifiers for Bindings {
    type ReferenceReceiver<T: 'static> = Slot<T>;
    type ReferenceNotifier<T: Send + 'static> = Slot<T>;

    fn new_reference_notifier<T: Send + 'static>(
        _debug_references: DynDebugReferences,
    ) -> (Slot<T>, Slot<T>) {
        let slot = Arc::new(Mutex::new(None));
        (Slot(Arc::clone(&slot)), Slot(slot))
    }
}

impl DeferredResourceRemovalContext for Bindings {
    fn defer_removal<T: Send + 'static>(&mut self, receiver: Slot<T>) {
        self.deferred.push(Box::new(receiver));
    }
}

/// A link resolution nobody waits on: netd's sockets learn of a failed
/// resolution from the core's own errors.
#[derive(Debug)]
pub struct Unobserved;

impl LinkResolutionContext<EthernetLinkDevice> for Bindings {
    type Notifier = Unobserved;
}

impl LinkResolutionNotifier<EthernetLinkDevice> for Unobserved {
    type Observer = ();

    fn new() -> (Self, ()) {
        (Unobserved, ())
    }

    fn notify(self, _result: Result<UnicastAddr<Mac>, netstack3_core::error::AddressResolutionFailed>) {}
}

impl<I: Ip> EventContext<IpDeviceEvent<DeviceId<Self>, I, StackTime>> for Bindings {
    fn on_event(&mut self, _event: IpDeviceEvent<DeviceId<Self>, I, StackTime>) {
        // Address and enablement changes netd made itself.
    }
}

impl<I: netstack3_core::IpExt> EventContext<IpLayerEvent<DeviceId<Self>, I>> for Bindings {
    fn on_event(&mut self, event: IpLayerEvent<DeviceId<Self>, I>) {
        unimplemented!("netd: an IP layer event it has no use for: {event:?}")
    }
}

impl<I: Ip> EventContext<netstack3_core::neighbor::Event<Mac, EthernetDeviceId<Self>, I, StackTime>>
    for Bindings
{
    fn on_event(&mut self, _event: netstack3_core::neighbor::Event<Mac, EthernetDeviceId<Self>, I, StackTime>) {
        // The neighbour table is the core's own.
    }
}

impl EventContext<RouterAdvertisementEvent<DeviceId<Self>>> for Bindings {
    fn on_event(&mut self, event: RouterAdvertisementEvent<DeviceId<Self>>) {
        unreachable!("netd runs with IPv6 disabled, and a router advertisement arrived: {event:?}")
    }
}

impl EventContext<EthernetDeviceEvent<EthernetDeviceId<Self>>> for Bindings {
    fn on_event(&mut self, event: EthernetDeviceEvent<EthernetDeviceId<Self>>) {
        match event {
            EthernetDeviceEvent::MulticastJoin { device: _, addr } => self.joined.push(addr.bytes()),
            EthernetDeviceEvent::MulticastLeave { device: _, addr } => {
                unimplemented!("netd never leaves a multicast group, and left {addr}")
            }
        }
    }
}

/// The core's allocator for frames it queues, which netd's device never does:
/// every frame goes to [`Egress`] as it is made.
pub struct VecAllocator;

impl TxBufferAllocator<Buf<Vec<u8>>> for VecAllocator {
    type Error = Infallible;

    fn alloc(&mut self, len: usize, _queue_len: usize) -> Result<Buf<Vec<u8>>, Infallible> {
        Ok(Buf::new(vec![0; len], ..))
    }
}

/// What a removal the core finished at once left, where netd holds no other
/// reference to the resource: a second one of its own would be netd's bug.
pub fn removed<R: 'static>(result: netstack3_core::sync::RemoveResourceResultWithContext<R, Bindings>) -> R {
    match result {
        netstack3_core::sync::RemoveResourceResult::Removed(r) => r,
        netstack3_core::sync::RemoveResourceResult::Deferred(_) => {
            panic!("netd: a socket it closed is still referenced elsewhere in netd")
        }
    }
}

impl<D> netstack3_core::filter::BindingsPacketMatcher<D> for Never {
    fn matches<I: FilterIpExt, P: FilterIpPacket<I>>(
        &self,
        _packet: &P,
        _interfaces: netstack3_core::filter::Interfaces<'_, D>,
        _meta: &impl netstack3_core::filter::FilterPacketMetadata,
    ) -> bool {
        match *self {}
    }
}
