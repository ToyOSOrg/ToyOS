//! A `drives` row's process for a test boot: it says which function the claim
//! it was started holding drives, by the address the kernel hands over, and
//! holds it until the boot ends.

use toyos::endow::{Endowments, DEV_PREFIX};
use toyos::PciDev;

/// What it says once it holds its function, followed by the function's
/// `bus:dev.func`.
const HOLDING: &str = "pciholder: holding PCI ";

fn main() {
    let endowed = Endowments::get();
    let mut labels = endowed.labels().filter(|label| label.strip_prefix(DEV_PREFIX).is_some_and(|name| name.starts_with("pci:")));
    let label = labels.next().map(str::to_string).expect("pciholder: started with no `dev:pci:` claim");
    assert!(labels.next().is_none(), "pciholder: started with two `dev:pci:` claims");
    let claim: PciDev = endowed.take(&label).expect("pciholder: the claim its label names");
    let info = claim.describe().expect("pciholder: its claim's description");
    println!("{HOLDING}{:02x}:{:02x}.{}", info.bus, info.dev, info.func);
    loop {
        std::thread::park();
    }
}
