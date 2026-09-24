//! The replacement a swap's reset control puts in netd's place: it takes the
//! `igb` the netd it replaces held — released by an Express function level
//! reset — and says what the function answers through the register window its
//! claim maps.
//!
//! Dword 0 is the one the kernel settled the window against when it placed
//! it, so an answer of all-zeroes or all-ones there is a window the function
//! no longer decodes.
//!
//! **It holds the claim until it is killed**: the host's verdict is the line
//! it prints, and the boot ends under it.

use toyos_abi::syscall::PciId;

/// QEMU's `igb`, the 82576.
const IGB: PciId = PciId { vendor: 0x8086, device: 0x10c9 };

fn main() {
    let Some(dev) = toyos::endow::pci_function::<toyos::PciDev>(IGB) else {
        println!("swap_flr_probe: started holding no igb");
        return;
    };
    let info = dev.describe().expect("swap_flr_probe: the claim's description");
    let (bar, bytes) = info
        .bar_bytes
        .iter()
        .enumerate()
        .find(|(_, bytes)| **bytes != 0)
        .map(|(index, bytes)| (index as u32, *bytes))
        .expect("swap_flr_probe: a claim with no window");
    let mapped = dev.map_bar(bar, bytes).expect("swap_flr_probe: the BAR");
    // SAFETY: the mapping is at least one dword long and lives until the
    // process is killed.
    let dword = unsafe { (mapped.as_ptr() as *const u32).read_volatile() };
    println!("swap_flr_probe: igb BAR {bar} dword 0 answers {dword:#010x}");
    loop {
        std::thread::park();
    }
}
