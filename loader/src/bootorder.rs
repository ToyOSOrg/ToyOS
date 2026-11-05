//! Where the machine goes after a reset: the firmware's own boot order.

use crate::efi::{cstr16, CStr16, SystemTable, GLOBAL_VARIABLE};

/// Whether the machine's default boot, `BootOrder`'s first entry, is another
/// entry than `BootCurrent`, the one that started this loader (UEFI 2.10
/// §3.3): a `BootNext` bought this boot, so a reset hands the machine to that
/// default and not back here. `false` where either variable does not read.
pub fn default_is_another(system_table: &SystemTable) -> bool {
    let rt = system_table.runtime_services();
    let read = |name: &CStr16| rt.get_variable(name, &GLOBAL_VARIABLE).ok().map(|(bytes, _)| bytes);
    let (Some(current), Some(order)) = (read(cstr16!("BootCurrent")), read(cstr16!("BootOrder"))) else {
        return false;
    };
    match (current.get(..2), order.get(..2)) {
        (Some(current), Some(first)) => current != first,
        _ => false,
    }
}
