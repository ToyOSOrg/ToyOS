//! Where the kernel drives the i8042, an `isa` claim on it is refused: two
//! drivers on one controller is not something a claim may create.

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
            "a claim on the i8042 answered {:?}, not PermissionDenied: the kernel does not drive \
             the controller here, or let a claim share it",
            other.map(|_| ())
        ),
    }
    println!("the claim on a controller the kernel drives was refused PermissionDenied");
}
