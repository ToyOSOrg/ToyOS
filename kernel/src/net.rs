use alloc::boxed::Box;
use alloc::vec::Vec;
use crate::inbox::InboxId;
use crate::sync::Lock;
use toyos_abi::syscall::SyscallError;

pub use toyos_abi::net::NicInfo;

/// Hardware-agnostic NIC driver interface.
pub trait Nic: Send {
    fn has_packet(&self) -> bool;

    /// Poll for a received frame without copying, returning `(buf_index, frame_len)`.
    fn poll_rx(&mut self) -> Option<(usize, usize)> { None }
    /// Resubmit an RX buffer to the hardware.
    fn refill_rx_buf(&mut self, _buf_index: usize) -> Result<(), SyscallError> {
        Err(SyscallError::NotSupported)
    }
    /// Submit the TX buffer to hardware.
    ///
    /// Frame data (with net header) must already be written before this is called.
    ///
    /// `total_len` must already be bounded by `tx_buf_len` before this is called (see `net::submit_tx`).
    fn submit_tx(&mut self, _total_len: usize) {}

    /// Size in bytes of the TX buffer userland writes into.
    fn tx_buf_len(&self) -> usize { 0 }
}

static NIC: Lock<Option<Box<dyn Nic>>> = Lock::new(None);
static NIC_INFO: Lock<Option<(NicInfo, crate::object::shm::Region)>> = Lock::new(None);
static INBOX_WATCHERS: Lock<Vec<InboxId>> = Lock::new(Vec::new());

pub fn add_inbox_watcher(id: InboxId) {
    let mut w = INBOX_WATCHERS.lock();
    if !w.contains(&id) { w.push(id); }
}

pub fn remove_inbox_watcher(id: InboxId) {
    INBOX_WATCHERS.lock().retain(|&x| x != id);
}

/// Wake every thread blocked on an incoming frame.
pub fn wake_waiters() {
    crate::sched::waitqs::wake_device(&crate::sched::waitqs::NETWORK_WATCH);
}

pub fn inbox_watchers() -> Vec<InboxId> {
    INBOX_WATCHERS.lock().clone()
}

pub fn register(nic: Box<dyn Nic>) {
    *NIC.lock() = Some(nic);
}

pub fn set_nic_info(info: NicInfo, dma: crate::object::shm::Region) {
    *NIC_INFO.lock() = Some((info, dma));
}

pub fn nic_info() -> Option<(NicInfo, crate::object::shm::Region)> {
    NIC_INFO.lock().clone()
}

pub fn has_packet() -> bool {
    NIC.lock().as_ref().is_some_and(|nic| nic.has_packet())
}

pub fn poll_rx() -> Option<(usize, usize)> {
    NIC.lock().as_mut().and_then(|nic| nic.poll_rx())
}

pub fn refill_rx_buf(buf_index: usize) -> Result<(), SyscallError> {
    let mut guard = NIC.lock();
    let Some(nic) = guard.as_mut() else { return Err(SyscallError::NotFound) };
    nic.refill_rx_buf(buf_index)
}

/// Hand the device the TX buffer's first `total_len` bytes.
///
/// This path has no pointer and no copy — the frame is written straight into shared DMA — so nothing else bounds `total_len`, which is why this check is the only bound.
pub fn submit_tx(total_len: usize) -> Result<(), SyscallError> {
    let mut guard = NIC.lock();
    let Some(nic) = guard.as_mut() else { return Err(SyscallError::NotFound) };
    if total_len == 0 || total_len > nic.tx_buf_len() {
        return Err(SyscallError::InvalidArgument);
    }
    nic.submit_tx(total_len);
    Ok(())
}
