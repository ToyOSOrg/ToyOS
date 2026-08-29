//! On a machine with no i8042 and no USB controller, a Keyboard or Mouse
//! claim must refuse `NotFound` — a claim is evidence, and this machine has
//! no hardware that could ever feed either stream. Driven by
//! `input_claim_absent` on the one bootable shape with no input source; on
//! every other machine both claims succeed, which is why it is in RUST_SKIP.

use toyos::device::{Keyboard, Mouse};
use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos_abi::syscall::{DeviceType, SyscallError, SYSCAP_LABEL};

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability");

    match cap.claim::<Keyboard>(DeviceType::Keyboard) {
        Err(SyscallError::NotFound) => println!("keyboard: refused NotFound"),
        Err(e) => panic!("keyboard claim: {e:?}, want NotFound"),
        Ok(_) => panic!("a keyboard claim succeeded on a machine with no input source"),
    }
    match cap.claim::<Mouse>(DeviceType::Mouse) {
        Err(SyscallError::NotFound) => println!("mouse: refused NotFound"),
        Err(e) => panic!("mouse claim: {e:?}, want NotFound"),
        Ok(_) => panic!("a mouse claim succeeded on a machine with no input source"),
    }
    println!("===INPUT_ABSENT_OK===");
}
