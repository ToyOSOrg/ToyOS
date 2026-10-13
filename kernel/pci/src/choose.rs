//! Which of the functions one `vendor:device` names is the one asked for: the
//! decision alone, over what the kernel read of each.
//!
//! **Two functions can carry one ID and be two different devices.** QEMU's
//! virtio-serial is `1af4:1043` whether it carries the kernel's single-port
//! console or a multiport channel a process drives, so the kernel's console
//! driver picks by what the device offers ([`console`]), and a claim picks
//! among the functions the kernel does not drive ([`claimed`]).

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

/// Why a claim's `vendor:device` names no function to hand over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unclaimable {
    /// No function carries it.
    Absent,
    /// Two or more functions the kernel does not drive carry it: the name asks
    /// for one of them and cannot say which.
    Ambiguous,
    /// Every function that carries it is the kernel's own.
    KernelDriven,
}

/// The function a claim resolves to, among those that carry its
/// `vendor:device` in enumeration order, each `true` where the kernel drives
/// it: the one function the kernel does not drive.
pub fn claimed(driven: impl IntoIterator<Item = bool>) -> Result<usize, Unclaimable> {
    let mut found = None;
    let mut any = false;
    for (at, driven) in driven.into_iter().enumerate() {
        any = true;
        if driven {
            continue;
        }
        if found.is_some() {
            return Err(Unclaimable::Ambiguous);
        }
        found = Some(at);
    }
    match (found, any) {
        (Some(at), _) => Ok(at),
        (None, true) => Err(Unclaimable::KernelDriven),
        (None, false) => Err(Unclaimable::Absent),
    }
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

    #[test]
    fn one_undriven_match_among_driven_ones_is_claimed() {
        assert_eq!(claimed([true, false, true]), Ok(1));
    }

    #[test]
    fn two_undriven_matches_are_ambiguous() {
        assert_eq!(claimed([false, true, false]), Err(Unclaimable::Ambiguous));
    }

    #[test]
    fn only_driven_matches_are_kernel_driven() {
        assert_eq!(claimed([true, true]), Err(Unclaimable::KernelDriven));
        assert_eq!(claimed([]), Err(Unclaimable::Absent));
    }
}
