//! A `drives` row's processes as the inventory sees them: each of QEMU's two
//! `edu` functions claimed, each by a `pciholder` of its own, still holding it
//! when this runs.

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::inventory::{Claim, Claimed, Driven, Pci, RawRecord, Record};

const EDU: (u16, u16) = (0x1234, 0x11e8);
const HOLDER: &str = "pciholder";

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("drives_judged: test-runner endows an inventory-reading capability");
    let records: Vec<Record> =
        cap.records(|n| vec![RawRecord::EMPTY; n]).unwrap_or_else(|why| panic!("drives_judged: the inventory: {why:?}"));
    let edus: Vec<&Pci> = records
        .iter()
        .filter_map(|record| match record {
            Record::Pci(pci) if (pci.vendor, pci.device) == EDU => Some(pci),
            _ => None,
        })
        .collect();
    assert_eq!(edus.len(), 2, "drives_judged: this machine has {} edu functions, not two", edus.len());
    let mut held = Vec::new();
    for edu in edus {
        let at = format!("{:02x}:{:02x}.{}", edu.at.bus, edu.at.dev, edu.at.func);
        assert_eq!(edu.driven, Driven::Claimed, "drives_judged: PCI {at} is not held");
        let holder = records
            .iter()
            .find_map(|record| match record {
                Record::Claim(Claim { on: Claimed::Pci(on), holder }) if *on == edu.at => Some(*holder),
                _ => None,
            })
            .unwrap_or_else(|| panic!("drives_judged: no claim record names PCI {at}"));
        assert_eq!(holder.name(), Some(HOLDER), "drives_judged: PCI {at} is held by {:?}", holder.name());
        held.push((at, holder.pid));
    }
    assert_ne!(held[0].1, held[1].1, "drives_judged: one process holds both functions: {held:?}");
    println!(
        "drives_judged: PCI {} held by {HOLDER} pid {}, PCI {} by {HOLDER} pid {}",
        held[0].0, held[0].1, held[1].0, held[1].1
    );
}
