//! A `drives` row's process for a test boot: it says which function the claim
//! it was started holding drives, by the address the kernel hands over, and
//! holds it until the function leaves. Then it says what each call on the
//! claim answered and ends with 0, which is owed no restart.

use toyos::endow::{Endowments, DEV_PREFIX};
use toyos::poller::{Poller, READABLE};
use toyos_abi::syscall::{RegWidth, SyscallError};
use toyos::PciDev;

/// What it says once it holds its function, followed by the function's
/// `bus:dev.func`.
const HOLDING: &str = "pciholder: holding PCI ";

/// What it says once its function has left, followed by the function's
/// `bus:dev.func` and each call's answer.
const GONE: &str = "pciholder: gone PCI ";

fn main() {
    let endowed = Endowments::get();
    let mut labels = endowed.labels().filter(|label| label.strip_prefix(DEV_PREFIX).is_some_and(|name| name.starts_with("pci:")));
    let label = labels.next().map(str::to_string).expect("pciholder: started with no `dev:pci:` claim");
    assert!(labels.next().is_none(), "pciholder: started with two `dev:pci:` claims");
    let claim: PciDev = endowed.take(&label).expect("pciholder: the claim its label names");
    let info = claim.describe().expect("pciholder: its claim's description");
    let at = format!("{:02x}:{:02x}.{}", info.bus, info.dev, info.func);
    println!("{HOLDING}{at}");
    let poller = Poller::new(1);
    let read = loop {
        poller.watch(&claim, READABLE, 0);
        poller.wait(1, u64::MAX, |_| {});
        match claim.irq() {
            Ok(_) | Err(SyscallError::WouldBlock) => continue,
            Err(why) => break why,
        }
    };
    let config = claim.config_read(0, RegWidth::U32).err();
    let bar = claim.map_bar(0, 1).err();
    let grant = claim.dma_alloc(4096).err();
    println!("{GONE}{at}: irq={read:?} config={config:?} bar={bar:?} dma={grant:?}");
}
