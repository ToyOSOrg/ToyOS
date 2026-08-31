//! A mode change that *succeeds*, which needs a display GOP is not: the new
//! framebuffer, the old one's release, the fresh scanout objects and
//! `device::set_framebuffer_info`'s update all live past the `NotSupported`
//! every other machine here answers with. The second claim is the point — what
//! the call returned is the driver talking about itself, and what a fresh
//! claim is told comes out of the registry the mode change had to update.

use std::time::Duration;

use toyos::device::FramebufferDev;
use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos_abi::syscall::{DeviceType, SyscallError, SYSCAP_LABEL};

/// Not the 1280x800 a virtio-gpu boots at, so the driver's "already this size"
/// early return cannot answer for the call.
const WANT: (u32, u32) = (800, 600);

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability");

    let fb: FramebufferDev =
        cap.claim(DeviceType::Framebuffer).expect("this machine has a display");
    let before = fb.info().expect("a claim describes the display it claimed");
    println!("gpu: claimed {}x{} stride={}", before.width, before.height, before.stride);
    assert_ne!(
        (before.width, before.height),
        WANT,
        "the machine already boots at the mode this asks for, so the call proves nothing",
    );

    let after = fb.set_resolution(WANT.0, WANT.1).expect("a virtio-gpu can change mode");
    assert_eq!((after.width, after.height), WANT, "the call answered another mode");
    assert!(after.stride >= WANT.0, "stride {} is under the width", after.stride);
    assert_ne!(
        after.scanout, before.scanout,
        "the answer names the old scanout objects, so nothing was reallocated",
    );
    // The new buffer reaches the device, which is what the host then reads.
    fb.present(0, 0, 0, 0).expect("present the new scanout");

    // Released, so the next description comes out of the registry.
    drop(fb);
    let again: FramebufferDev = reclaim(&cap);
    let told = again.info().expect("the second claim describes the display");
    assert_eq!(
        (told.width, told.height, told.stride),
        (after.width, after.height, after.stride),
        "the call said {}x{} stride={} and a second claim is told {}x{} stride={}",
        after.width,
        after.height,
        after.stride,
        told.width,
        told.height,
        told.stride,
    );

    println!(
        "gpu: set {}x{} stride={}, and a second claim is told the same",
        told.width, told.height, told.stride
    );
    println!("===GPU_RESOLUTION_OK===");
}

/// The claim again, once the release the last close *queued* has run:
/// `object::drain_zero_handles` says a syscall may return to userland before
/// its own releases finish, so `AlreadyExists` is that queue, not a holder.
fn reclaim(cap: &SysCap) -> FramebufferDev {
    for _ in 0..RECLAIM_TRIES {
        match cap.claim(DeviceType::Framebuffer) {
            Ok(fb) => return fb,
            Err(SyscallError::AlreadyExists) => std::thread::sleep(RECLAIM_STEP),
            Err(e) => panic!("the second claim was refused {e:?}"),
        }
    }
    panic!(
        "the framebuffer claim was still held {:?} after its handle closed",
        RECLAIM_STEP * RECLAIM_TRIES,
    );
}

/// Five seconds over a queue drained at every syscall exit: exhausting it is a
/// defect, not a slow host.
const RECLAIM_TRIES: u32 = 500;
const RECLAIM_STEP: Duration = Duration::from_millis(10);
