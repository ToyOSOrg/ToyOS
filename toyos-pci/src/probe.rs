//! Which dword of a moved BAR a function answers a value of its own at.
//!
//! Moving a BAR is proved by the function answering the same value at the new
//! address as at the one firmware gave it, so the dword that is read has to be
//! one whose value is the function's. `0x00000000` and `0xFFFFFFFF` are not:
//! they are what a read nobody answers comes back as, on an emulator and on a
//! root complex respectively, so a function whose reference dword is either of
//! them settles nothing and is refused by name.
//!
//! **The first dword a BAR's base names is the default**, because it is the one
//! offset every memory BAR has. A function whose identity says that dword is a
//! selector rather than a value names the one that carries the value instead.

use toyos_abi::syscall::PciId;

/// Virtio's own PCI vendor id (virtio 1.2 §4.1.2).
const VIRTIO: u16 = 0x1af4;

/// The modern virtio device ids: `0x1040 + <device type>` (virtio 1.2 §4.1.2.1).
const VIRTIO_MODERN: core::ops::RangeInclusive<u16> = 0x1040..=0x107f;

/// A virtio common configuration structure opens with `device_feature_select`,
/// which reads zero, and carries `device_feature` in the dword after it
/// (virtio 1.2 §4.1.4.3).
const VIRTIO_DEVICE_FEATURE: u64 = 4;

/// The byte offset into a moved BAR of the dword `id` answers a value of its
/// own at.
///
/// Every offset here is inside the smallest memory BAR the spec allows — the
/// low four bits of one are its type field, so sixteen bytes is its floor
/// (PCI 3.0 §6.2.5.1) — so no caller has to bound it against the BAR's size.
pub fn reference(id: PciId) -> u64 {
    match id {
        PciId { vendor: VIRTIO, device } if VIRTIO_MODERN.contains(&device) => {
            VIRTIO_DEVICE_FEATURE
        }
        _ => 0,
    }
}

/// Whether `dword` says nothing about who answered it.
pub fn degenerate(dword: u32) -> bool {
    dword == 0 || dword == u32::MAX
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two functions this project hands to a process, and a third the table
    /// knows nothing about.
    #[test]
    fn a_function_the_table_knows_nothing_about_is_read_at_its_base() {
        assert_eq!(reference(PciId { vendor: 0x1af4, device: 0x1041 }), 4);
        assert_eq!(reference(PciId { vendor: 0x8086, device: 0x15fc }), 0);
        assert_eq!(reference(PciId { vendor: 0x8086, device: 0x10d3 }), 0);
    }

    /// Virtio's transitional ids are below the modern range and are not it.
    #[test]
    fn a_transitional_virtio_id_is_not_a_modern_one() {
        assert_eq!(reference(PciId { vendor: VIRTIO, device: 0x1000 }), 0);
        assert_eq!(reference(PciId { vendor: VIRTIO, device: 0x103f }), 0);
        assert_eq!(reference(PciId { vendor: VIRTIO, device: 0x1080 }), 0);
    }

    /// Both of the two answers a read nobody answered comes back as.
    #[test]
    fn neither_answer_of_an_unanswered_read_is_a_reference() {
        assert!(degenerate(0x0000_0000));
        assert!(degenerate(0xFFFF_FFFF));
        assert!(!degenerate(0x0018_0240));
        assert!(!degenerate(1));
    }
}
