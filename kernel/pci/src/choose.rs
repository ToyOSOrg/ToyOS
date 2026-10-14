//! Which of the functions one `vendor:device` names the kernel's console
//! drives: the decision alone, over what the kernel read of each.
//!
//! **Two functions can carry one ID and be two different devices.** QEMU's
//! virtio-serial is `1af4:1043` whether it carries the kernel's single-port
//! console or a multiport channel a process drives, so the console driver
//! picks by what the device offers.

/// Virtio 1.2 §5.3.3: the console device's `VIRTIO_CONSOLE_F_MULTIPORT`.
pub const VIRTIO_CONSOLE_F_MULTIPORT: u64 = 1 << 1;

/// The function the kernel's console drives, among console functions whose
/// device features are `features`, in enumeration order: the first that does
/// not offer `VIRTIO_CONSOLE_F_MULTIPORT`.
///
/// A single-port driver drives nothing else. A multiport device names its
/// console ports by `CONSOLE_PORT` messages on control queues (§5.3.6.2),
/// which a single-port driver has none of.
pub fn console(features: &[u64]) -> Option<usize> {
    features.iter().position(|offered| offered & VIRTIO_CONSOLE_F_MULTIPORT == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `VIRTIO_F_VERSION_1` beside each, as QEMU offers it on both functions.
    const VERSION_1: u64 = 1 << 32;
    const SINGLE: u64 = VERSION_1;
    const MULTI: u64 = VERSION_1 | VIRTIO_CONSOLE_F_MULTIPORT;

    #[test]
    fn the_console_is_the_first_function_without_multiport() {
        assert_eq!(console(&[MULTI, SINGLE, SINGLE]), Some(1));
    }

    #[test]
    fn multiport_functions_alone_give_no_console() {
        assert_eq!(console(&[MULTI, MULTI]), None);
        assert_eq!(console(&[]), None);
    }
}
