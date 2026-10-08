//! One memory BAR of a claimed function, asked for again on the same live
//! claim after the handle an earlier answer gave is gone: the answer is a
//! handle to an object of its own, and the function's registers are behind its
//! mapping. Twice — after a close, and after an ask a full handle table
//! refused, which is an object whose only handle went before its holder ever
//! had it.
//!
//! The function is QEMU's virtio NIC, which no program of `tests/testcases`
//! holds.

use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos::{AsHandle, PciDev};
use toyos_abi::handle::RawHandle;
use toyos_abi::syscall::{self, PciId, SyscallError, SYSCAP_LABEL};

const NIC: PciId = PciId { vendor: 0x1af4, device: 0x1041 };

/// `device_feature` of the common configuration QEMU puts at the base of the
/// NIC's one mappable BAR (virtio 1.2 §4.1.4.3): a dword that is the device's
/// own, never zero for a NIC, and that no read changes.
const DEVICE_FEATURE: usize = 4;

/// What the function answers through a fresh ask of `bar`, with the handle
/// and the mapping let go again before this returns.
fn ask(nic: &PciDev, bar: u32, bytes: u64) -> u32 {
    let window = nic.map_bar(bar, bytes).expect("bar_map_again: the BAR, asked for and mapped");
    // SAFETY: `map_bar` answered `bytes` bytes of live mapping, the dword is
    // inside the sixteen a memory BAR has at least, and a device register is
    // read by a volatile load.
    let answered = unsafe { window.as_ptr().add(DEVICE_FEATURE).cast::<u32>().read_volatile() };
    assert!(
        answered != 0 && answered != u32::MAX,
        "bar_map_again: the mapping answers {answered:#010x}, which is no device's register"
    );
    answered
}

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability");
    let nic: PciDev = cap.claim_pci(NIC).expect("bar_map_again: the virtio NIC's claim");
    let info = nic.describe().expect("bar_map_again: the claim's description");
    let bar = info
        .bar_bytes
        .iter()
        .position(|&bytes| bytes != 0)
        .expect("bar_map_again: the function has a memory BAR its holder may map");
    let bytes = info.bar_bytes[bar];
    let bar = bar as u32;

    let first = ask(&nic, bar, bytes);
    println!("bar_map_again: BAR {bar} asked for and its handle closed; asking again");
    let again = ask(&nic, bar, bytes);
    assert_eq!(again, first, "bar_map_again: the second ask's mapping is another window");
    println!("bar_map_again: answered with a handle that maps");

    // No room for the handle: the ask is refused, and the object made for it
    // went with the handle the table would not take.
    // Standard output fills the table: a claim's own handle carries no `DUP`.
    let mut filled = Vec::new();
    let full = loop {
        match syscall::dup(RawHandle(1)) {
            Ok(handle) => filled.push(handle),
            Err(why) => break why,
        }
    };
    let refused = syscall::device_bar_map(nic.as_handle(), bar);
    for handle in filled {
        syscall::close(handle);
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
}
