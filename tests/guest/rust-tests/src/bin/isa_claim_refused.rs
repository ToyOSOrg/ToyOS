//! Where the kernel declared a port of the i8042's row, an `isa` claim on the
//! row is refused: a claim may not hand a process a port the kernel drives.
//!
//! Run under `isa-port-declared`, which declares the command port to the
//! kernel as a FADT reset register at 0x64 is.

use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos::Device;
use toyos_abi::syscall::{IsaId, SyscallError, SYSCAP_LABEL};

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability");
    let set = IsaId::parse("0060,0064:1,12").expect("the i8042's set");
    match cap.claim_isa::<Device>(set) {
        Err(SyscallError::PermissionDenied) => {}
        other => panic!(
            "a claim on a row whose port the kernel declared answered {:?}, not PermissionDenied",
            other.map(|_| ())
        ),
    }
    println!("the claim on a row with a port the kernel declared was refused PermissionDenied");
}
