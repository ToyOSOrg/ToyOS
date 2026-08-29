//! A Keyboard or Mouse claim must refuse `NotFound` on a machine with no
//! i8042 and no USB controller. Driven by `input_claim_absent` alone: on
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
