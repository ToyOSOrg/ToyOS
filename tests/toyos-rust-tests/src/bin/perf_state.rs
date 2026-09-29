//! The performance-state claim. Where the machine declared no request the
//! claim is refused `NotFound`; where it declared one, `/system/bin/perfstate`'s
//! own read answers every CPU holding its declaration, twice. Which branch is
//! right is the machine's to say, so `perf_request` drives it and reads which
//! one it took.

use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos::Device;
use toyos_abi::perf::answer_len;
use toyos_abi::syscall::{self, DeviceType, SyscallError, SYSCAP_LABEL};

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability");
    match cap.claim::<Device>(DeviceType::PerfState) {
        Err(SyscallError::NotFound) => println!("perf-state: refused NotFound"),
        Err(e) => panic!("perf-state claim: {e:?}, want NotFound or a claim"),
        Ok(claim) => {
            let mut short = vec![0u8; answer_len(syscall::cpu_count() as usize) - 1];
            assert_eq!(claim.read(&mut short), Err(SyscallError::ResourceExhausted));
            // Twice: an answered read leaves nothing outstanding for the next.
            for _ in 0..2 {
                if let Err(why) = perfstate::read_back(&claim) {
                    panic!("{why}");
                }
            }
            println!("perf-state: declared");
        }
    }
    println!("===PERF_STATE_OK===");
}
