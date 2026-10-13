//! What the kernel's HDA stub hands its driver.
//!
//! The line through the device is **who touches a register**: the kernel
//! programs every register whose value is an address or indexes a structure it
//! allocated, and the driver reaches the rest through
//! [`syscall::device_reg_read`] and [`syscall::device_reg_write`], each checked
//! against an allow-list and refused by name. Nothing here names a physical
//! address.
//!
//! **The kernel binds no controller by itself.** It lists every controller it
//! can bind and takes the stream's memory once; the driver walks each listed
//! controller's codecs over the register calls, chooses one, and binds it with
//! [`syscall::hda_bind`]. From then on the claim's list is the bound
//! controller alone, at index 0.
//!
//! Completions come back as [`AudioCompletionRecord`](crate::audio::AudioCompletionRecord),
//! whose mask is derived from a position read in the interrupt handler.
//!
//! [`syscall::device_reg_read`]: crate::syscall::device_reg_read
//! [`syscall::device_reg_write`]: crate::syscall::device_reg_write
//! [`syscall::hda_bind`]: crate::syscall::hda_bind

/// The most controllers a claim lists. A machine with more live ones is
/// refused by name, naming each.
pub const MAX_CONTROLLERS: usize = 4;

crate::user_safe! {
    /// One controller the claim lists.
    #[derive(Clone, Copy)]
    pub struct HdaController {
        /// Byte offset of the output stream descriptor inside this controller's
        /// register window, so the driver names `SDnCTL` and `SDnFMT` by the
        /// same arithmetic the allow-list does.
        pub stream_offset: u32,
        /// Every codec address `STATESTS` reported, one bit per link address.
        /// The driver enumerates all of them and chooses by capability; the
        /// kernel read this register to know the link is alive and decided
        /// nothing with it.
        pub statests: u16,
        /// The PCI bus, device and function, for the lines that name it.
        pub bus: u8,
        pub dev: u8,
        pub func: u8,
        pub _pad: [u8; 3],
    }
}

crate::user_safe! {
    /// The stream the kernel took the memory of, and the controllers it can be
    /// bound to, as the driver needs to see them.
    ///
    /// No register window and no physical address: everything here is a shared
    /// memory token, a shape the driver has to know to fill the ring, or a number
    /// it has to send a codec.
    #[derive(Clone, Copy)]
    pub struct HdaInfo {
        /// The PCM ring, mapped writable. `periods` buffers of `period_bytes` laid
        /// end to end from the start of the region, with the buffer descriptor
        /// list already pointing at them.
        pub pcm: crate::RawHandle,
        pub period_bytes: u32,
        /// The stream tag the kernel puts in the descriptor. It has to reach the
        /// codec's converter, and sending that verb is the driver's.
        pub stream_tag: u8,
        pub periods: u8,
        /// How many of [`controllers`](Self::controllers) are listed: every
        /// controller the kernel can bind, in PCI order, or the bound one
        /// alone once a bind has run.
        pub count: u8,
        pub _pad: u8,
        pub controllers: [HdaController; MAX_CONTROLLERS],
    }
}

impl HdaInfo {
    /// The listed controllers.
    ///
    /// A `count` past [`MAX_CONTROLLERS`] is the kernel's bug, and it panics
    /// here rather than reading past the list.
    pub fn listed(&self) -> &[HdaController] {
        &self.controllers[..self.count as usize]
    }
}

/// One register of one listed controller: its index in the claim's current
/// list, and a byte offset inside that controller's register window.
///
/// The register calls carry it as their offset word, decoded once at the
/// boundary by [`Reg::from_wire`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reg {
    pub controller: u8,
    pub offset: u32,
}

impl Reg {
    /// The offset word the register calls carry: the offset in the low 32
    /// bits, the controller's index above them.
    pub const fn wire(self) -> u64 {
        (self.controller as u64) << 32 | self.offset as u64
    }

    /// The register a word names, or `None` for a word whose controller index
    /// no list can hold.
    pub const fn from_wire(word: u64) -> Option<Self> {
        let controller = word >> 32;
        if controller >= MAX_CONTROLLERS as u64 {
            return None;
        }
        Some(Self { controller: controller as u8, offset: word as u32 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_register_word_is_refused_unless_its_controller_index_fits_a_list() {
        for controller in 0..MAX_CONTROLLERS as u8 {
            let reg = Reg { controller, offset: 0x68 };
            assert_eq!(Reg::from_wire(reg.wire()), Some(reg));
        }
        assert_eq!(Reg::from_wire((MAX_CONTROLLERS as u64) << 32 | 0x68), None);
        assert_eq!(Reg::from_wire(1 << 40), None);
        assert_eq!(Reg::from_wire(u64::MAX), None);
    }
}
