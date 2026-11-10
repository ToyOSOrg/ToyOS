//! One memory BAR of a claimed function, asked for more than once on the same
//! live claim: every answer is a handle to an object of its own, and the
//! function's registers are behind its mapping. Three times — while an earlier
//! answer is still held, where the two are two mappings and the later one
//! outlives the earlier; after a close; and after an ask a full handle table
//! refused, which is an object whose only handle went before its holder ever
//! had it. And a BAR handle kept past its claim's move, here over a
//! connection to this process itself, maps nothing, while the claim's new
//! holder maps the BAR; and one kept past the claim's end maps nothing. And a
//! syscall naming the mapping as its buffer copies through it, making the
//! byte accesses its holder could.
//!
//! The function is QEMU's virtio NIC, which no program of `tests/images/testcases.toml`
//! holds.

#[path = "../function.rs"]
mod function;

use toyos::endow::Endowments;
use toyos::ipc::FrameRx;
use toyos::shm::SharedMemory;
use toyos::syscap::SysCap;
use toyos::{namespace, port, AsHandle, OwnedHandle, PciDev};
use toyos_abi::handle::RawHandle;
use toyos_abi::syscall::{self, PciId, SyscallError, SYSCAP_LABEL};

const NIC: PciId = PciId { vendor: 0x1af4, device: 0x1041 };

/// The BAR QEMU's virtio-pci puts the modern structures in, the common
/// configuration at its base: BAR 1 holds the MSI-X table.
const COMMON: u32 = 4;

/// `device_feature` of that common configuration (virtio 1.2 §4.1.4.3): a
/// dword that is the device's own, never zero for a NIC, and that no read
/// changes.
const DEVICE_FEATURE: usize = 4;

/// `device_feature_select` of the same structure: read back as written, and
/// zero wherever `device_feature` is read here.
const DEVICE_FEATURE_SELECT: usize = 0;

/// What the function answers through `window`, a live mapping of its BAR.
fn read(window: &SharedMemory) -> u32 {
    // SAFETY: `map_bar` answered a live mapping of the BAR, the dword is inside
    // the sixteen bytes a memory BAR has at least, and a device register is read
    // by a volatile load.
    let answered = unsafe { window.as_ptr().add(DEVICE_FEATURE).cast::<u32>().read_volatile() };
    assert!(
        answered != 0 && answered != u32::MAX,
        "bar_map_again: the mapping answers {answered:#010x}, which is no device's register"
    );
    answered
}

/// A pipe write whose buffer is `device_feature` carries what this process's
/// own byte loads read there, and a pipe read whose buffer is
/// `device_feature_select` stores its byte in the register.
fn copied_through(window: &SharedMemory) {
    let base = window.as_ptr();
    let pipe = syscall::pipe().expect("bar_map_again: a pipe");

    // SAFETY: the dword is inside the live mapping; the slice only carries
    // its address into the syscall.
    let feature = unsafe { core::slice::from_raw_parts(base.add(DEVICE_FEATURE), 4) };
    assert_eq!(syscall::write(pipe.write, feature), Ok(4), "bar_map_again: a write from the BAR");
    let mut copied = [0u8; 4];
    assert_eq!(syscall::read(pipe.read, &mut copied), Ok(4), "bar_map_again: the pipe gave no bytes back");
    // SAFETY: each byte is inside the live mapping, read by a volatile load.
    let own: [u8; 4] = core::array::from_fn(|i| unsafe { base.add(DEVICE_FEATURE + i).read_volatile() });
    assert_eq!(copied, own, "bar_map_again: the write from the BAR carried other bytes than its holder reads");

    assert_eq!(syscall::write(pipe.write, &[1]), Ok(1), "bar_map_again: the pipe took no byte");
    // SAFETY: the byte is inside the live mapping; the slice only carries its
    // address into the syscall.
    let select = unsafe { core::slice::from_raw_parts_mut(base.add(DEVICE_FEATURE_SELECT), 1) };
    assert_eq!(syscall::read(pipe.read, select), Ok(1), "bar_map_again: a read into the BAR");
    // SAFETY: the byte is inside the live mapping, read and written volatile.
    let selected = unsafe { base.add(DEVICE_FEATURE_SELECT).read_volatile() };
    // SAFETY: as above. Put back before the judgement: `read` wants word 0.
    unsafe { base.add(DEVICE_FEATURE_SELECT).write_volatile(0) };
    assert_eq!(selected, 1, "bar_map_again: the read into the BAR did not reach the register");

    // SAFETY: no type holds this number.
    unsafe { syscall::close(pipe.read) };
    // SAFETY: no type holds this number.
    unsafe { syscall::close(pipe.write) };
}

/// What the function answers through a fresh ask of `bar`, with the handle
/// and the mapping let go again before this returns.
fn ask(nic: &PciDev, bar: u32, bytes: u64) -> u32 {
    read(&nic.map_bar(bar, bytes).expect("bar_map_again: the BAR, asked for and mapped"))
}

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability");
    let nic: PciDev = function::claim(&cap, NIC).expect("bar_map_again: the virtio NIC's claim");
    let info = nic.describe().expect("bar_map_again: the claim's description");
    let bar = COMMON;
    let bytes = info.bar_bytes[bar as usize];
    assert!(bytes != 0, "bar_map_again: the NIC's BAR {bar} is not one its holder may map");

    // Two answers held at once are two mappings, and letting the earlier one
    // go takes nothing from the later.
    let earlier = nic.map_bar(bar, bytes).expect("bar_map_again: the BAR, asked for and mapped");
    let later = nic.map_bar(bar, bytes).expect("bar_map_again: the BAR, asked for while held");
    assert_ne!(
        earlier.as_ptr(),
        later.as_ptr(),
        "bar_map_again: two answers held at once are one mapping"
    );
    let first = read(&earlier);
    assert_eq!(read(&later), first, "bar_map_again: the two mappings are two windows");
    drop(earlier);
    assert_eq!(read(&later), first, "bar_map_again: the later mapping changed with the earlier's end");
    drop(later);
    println!("bar_map_again: two answers held at once map apart, and the later outlives the earlier");

    println!("bar_map_again: BAR {bar} asked for and its handle closed; asking again");
    let again = ask(&nic, bar, bytes);
    assert_eq!(again, first, "bar_map_again: the second ask's mapping is another window");
    println!("bar_map_again: answered with a handle that maps");

    copied_through(&nic.map_bar(bar, bytes).expect("bar_map_again: the BAR, asked for and mapped"));
    println!("bar_map_again: a syscall's copies through the BAR were its holder's own accesses");

    // No room for the handle: the ask is refused, and the object made for it
    // went with the handle the table would not take.
    // Standard output fills the table: a claim's own handle carries no `DUP`.
    let mut filled = Vec::new();
    let full = loop {
        // SAFETY: nothing relies on being the source's only holder.
        match unsafe { syscall::dup(stdout()) } {
            Ok(handle) => filled.push(handle),
            Err(why) => break why,
        }
    };
    let refused = syscall::device_bar_map(nic.as_handle().raw(), bar);
    for handle in filled {
        // SAFETY: no type holds this number.
        unsafe { syscall::close(handle) };
    }
    // Judged once the table has room again, for the report.
    assert_eq!(full, SyscallError::ResourceExhausted, "bar_map_again: the table did not fill");
    assert_eq!(
        refused,
        Err(SyscallError::ResourceExhausted),
        "bar_map_again: an ask from a full table was not refused for room"
    );
    println!("bar_map_again: an ask from a full handle table refused; asking again");
    let after = ask(&nic, bar, bytes);
    assert_eq!(after, first, "bar_map_again: the ask after the refusal maps another window");
    println!("bar_map_again: answered after the refusal with a handle that maps");

    // A BAR's handle kept past its claim's move maps nothing: the move revoked
    // every aperture the claim had handed out. The claim's new holder, which
    // is this process again, maps the BAR afresh.
    let kept = syscall::device_bar_map(nic.as_handle().raw(), bar).expect("bar_map_again: the BAR, asked for");
    let (acceptor, connector) = port::create().expect("bar_map_again: a port");
    let names = namespace::build().add("conn", &connector).finish().expect("bar_map_again: a namespace");
    let client = names.open("conn").expect("bar_map_again: a connection");
    let server = acceptor.accept().expect("bar_map_again: the connection accepted");
    let claim = nic.as_handle().raw();
    // The claim's handle goes with the frame: nothing may close it here.
    core::mem::forget(nic);
    // SAFETY: the claim's live handle, which the forgotten `PciDev` no longer
    // answers for.
    client.signal_with(1, [unsafe { OwnedHandle::from_raw(claim) }]).expect("bar_map_again: the claim sent");
    assert_eq!(map(kept).err(), Some(SyscallError::Gone), "bar_map_again: a BAR's handle mapped after its claim moved");
    let mut rx = FrameRx::<0>::new();
    let frame = rx.recv(&server).expect("bar_map_again: the claim received");
    assert_eq!(frame.handles, 1, "bar_map_again: the frame carries no claim");
    let [claim] = rx.take_handles::<1>().expect("bar_map_again: the claim's handle");
    let fresh = syscall::device_bar_map(claim.as_handle().raw(), bar).expect("bar_map_again: the BAR, asked for on the moved claim");
    let window = map(fresh).expect("bar_map_again: the moved claim's BAR mapped");
    // SAFETY: as in `read`, through the mapping just made.
    let answered = unsafe { window.add(DEVICE_FEATURE).cast::<u32>().read_volatile() };
    assert_eq!(answered, first, "bar_map_again: the moved claim's mapping is another window");
    println!("bar_map_again: a BAR's handle kept past its claim's move maps nothing, and the new holder's maps");

    // A BAR's handle kept past its claim's end maps nothing: the claim's end
    // revoked every aperture it handed out.
    let kept = syscall::device_bar_map(claim.as_handle().raw(), bar).expect("bar_map_again: the BAR, asked for");
    drop(claim);
    assert_eq!(map(kept).err(), Some(SyscallError::Gone), "bar_map_again: a BAR's handle mapped after its claim ended");
    println!("bar_map_again: a BAR's handle kept past its claim's end maps nothing");
}

/// `shm` mapped, or why not.
fn map(shm: RawHandle) -> Result<*mut u8, SyscallError> {
    // SAFETY: a refused map makes no mapping; one made is only read, as a
    // device register.
    unsafe { syscall::shm_map(shm, toyos_abi::syscall::VmProt::ReadWrite) }
}

/// The kernel handle this process's standard output is, descriptor 1's.
fn stdout() -> RawHandle {
    toyos::fd::with(1, |o| o.handle().map(toyos::BorrowedHandle::raw)).ok().flatten().expect("the test is given a standard output")
}
