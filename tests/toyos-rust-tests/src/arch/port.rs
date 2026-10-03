//! `in` and `out` from Ring 3, on the one architecture with a port space.

#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(target_arch = "x86_64")]
mod x86_64 {
    /// `in al, dx`, or `in ax, dx` when `wide`, which spans `port` and the port
    /// after it. A port this process holds no grant for faults it.
    pub fn port_in(port: u16, wide: bool) -> u16 {
        let value: u16;
        // SAFETY: an `in` has no memory effect.
        unsafe {
            if wide {
                core::arch::asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack));
            } else {
                core::arch::asm!("in al, dx", in("dx") port, out("ax") value, options(nomem, nostack));
            }
        }
        if wide { value } else { value & 0xFF }
    }

    /// `out dx, al`. A port this process holds no grant for faults it.
    pub fn port_out(port: u16, value: u8) {
        // SAFETY: an `out` has no memory effect; what the device does with the
        // byte is the caller's.
        unsafe {
            core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
        }
    }
}
