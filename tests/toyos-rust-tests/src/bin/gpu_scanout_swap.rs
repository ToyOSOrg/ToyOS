//! Three mode changes in a row while this process keeps every scanout it was
//! ever handed mapped and stamped. The pages a swap retires live as long as a
//! holder maps them; what the device may still reach is the host's question,
//! answered by `iommu_gpu_scanout_swap` over the tables the unit walks.

use toyos::device::FramebufferDev;
use toyos::endow::Endowments;
use toyos::shm::SharedMemory;
use toyos::syscap::SysCap;
use toyos_abi::syscall::{DeviceType, SYSCAP_LABEL};

/// Mirrored in `tests/common/iommu.rs`; none is the mode a virtio-gpu boots at.
const MODES: [(u32, u32); 3] = [(800, 600), (1024, 768), (640, 480)];
const STAMP: [u8; 8] = *b"scanout!";

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability");
    let fb: FramebufferDev =
        cap.claim(DeviceType::Framebuffer).expect("this machine has a display");
    let mut info = fb.info().expect("a claim describes the display it claimed");
    println!("gpu: claimed {}x{} stride={}", info.width, info.height, info.stride);

    let mut held: Vec<SharedMemory> = Vec::new();
    for &(width, height) in &MODES {
        assert_ne!((info.width, info.height), (width, height), "already in the mode asked for");
        let bytes = (info.stride * info.height * 4) as usize;
        let mut front = SharedMemory::adopt(info.scanout[0], bytes)
            .expect("the scanout buffer the description just handed over");
        front.as_mut_slice()[..STAMP.len()].copy_from_slice(&STAMP);
        held.push(front);

        info = fb.set_resolution(width, height).expect("a virtio-gpu can change mode");
        assert_eq!((info.width, info.height), (width, height), "the call answered another mode");
        fb.present(0, 0, 0, 0).expect("present the new scanout");

        for (n, buffer) in held.iter().enumerate() {
            assert_eq!(
                &buffer.as_slice()[..STAMP.len()],
                &STAMP,
                "scanout {n} is not this process's any more after the change to {width}x{height}",
            );
        }
        println!(
            "gpu: {width}x{height} set, {} retired scanout(s) still mapped and stamped",
            held.len()
        );
    }
    println!("===GPU_SCANOUT_SWAP_OK===");
}
