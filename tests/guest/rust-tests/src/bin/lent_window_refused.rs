//! A file server believes the kernel about the window a client lends it, never
//! the client: a hello whose window is no region, a region longer than
//! `WINDOW_BYTES`, or a device's registers is refused, and the server that
//! refused it answers the next client as the same process.
//!
//! The device is QEMU's virtio NIC, which no program of
//! `tests/images/testcases.toml` holds: its BAR is the one region a test can
//! lend that is not memory the kernel allocated.

use toyos::endow::Endowments;
use toyos::fs::{Reply, Request, HELLO, REPLY, WINDOW_BYTES};
use toyos::ipc::IpcError;
use toyos::shm::SharedMemory;
use toyos::syscap::SysCap;
use toyos::{AsHandle, OwnedHandle, PciDev};
use toyos_abi::syscall::{self, PciId, SyscallError, SYSCAP_LABEL};

const NIC: PciId = PciId { vendor: 0x1af4, device: 0x1041 };

/// What `fs:/home` answers a hello lending `window`: the volume, which is the
/// server's pid, or `Gone` for a server that let the client go unanswered.
fn hello(window: OwnedHandle) -> Result<u64, SyscallError> {
    let names = toyos::endow::namespace().expect("lent_window_refused: a namespace");
    let conn = names.open("fs:/home").expect("lent_window_refused: fs:/home");
    conn.send_handles([window]).expect("lent_window_refused: the window into the connection");
    conn.send(HELLO, &Request::new()).expect("lent_window_refused: the hello");
    let header = match conn.recv_header() {
        Ok(header) => header,
        Err(IpcError::Disconnected | IpcError::Syscall(SyscallError::Gone)) => return Err(SyscallError::Gone),
        Err(e) => panic!("lent_window_refused: the hello's answer: {e:?}"),
    };
    assert_eq!(header.msg_type, REPLY, "lent_window_refused: a hello answered by a frame that is no reply");
    let reply: Reply = conn.recv_payload(&header).expect("lent_window_refused: the reply's words");
    assert_eq!(reply.status, 0, "lent_window_refused: a hello refused with a word, not let go");
    Ok(reply.value2)
}

fn region(bytes: usize) -> OwnedHandle {
    SharedMemory::create(bytes).expect("lent_window_refused: a region").share().expect("lent_window_refused: its handle")
}

fn main() {
    let served = hello(region(WINDOW_BYTES)).expect("lent_window_refused: a window of WINDOW_BYTES is served");

    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("lent_window_refused: a device-minting capability");
    let nic: PciDev = cap.claim_pci(NIC).expect("lent_window_refused: the virtio NIC's claim");
    let info = nic.describe().expect("lent_window_refused: the claim's description");
    let bar = info.bar_bytes.iter().position(|&bytes| bytes != 0).expect("lent_window_refused: a memory BAR");
    let bar = syscall::device_bar_map(nic.as_handle(), bar as u32).expect("lent_window_refused: the BAR's region");
    let (read, _write) = toyos::pipe_pair().expect("lent_window_refused: a pipe");

    // Every hostile hello is made before any is judged, so a server that serves one is seen serving each.
    let answers = [
        ("a pipe end", hello(read.into())),
        ("a region twice WINDOW_BYTES long", hello(region(2 * WINDOW_BYTES))),
        // SAFETY: the kernel installed the handle for this call, and nothing else answers for it.
        ("a device's BAR", hello(unsafe { OwnedHandle::from_raw(bar) })),
    ];
    let again = hello(region(WINDOW_BYTES)).expect("lent_window_refused: a window after the refusals is served");

    for (what, answer) in &answers {
        println!("lent_window_refused: a hello lending {what} answered {answer:?}");
    }
    assert!(
        answers.iter().all(|(_, answer)| *answer == Err(SyscallError::Gone)),
        "lent_window_refused: a hostile hello was not let go unanswered"
    );
    assert_eq!(again, served, "lent_window_refused: the server that refused is not the one that answers");
    println!("lent_window_refused: the server that refused answers the next client, volume {served}");
}
