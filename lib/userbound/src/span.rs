//! The arithmetic every user-memory access rests on: the user/kernel bound, the
//! alignment a type needs, and whether a value the kernel is about to
//! dereference lies inside one mapping.
//!
//! **Three refusals, and each has a way of being wrong the others do not
//! catch.** An address in the kernel half is refused outright. An object whose
//! address is userland's but whose alignment is wrong is refused, because a
//! misaligned read of a wider type reads bytes the caller never proved were
//! mapped. An object that starts inside one 2 MiB page and ends in the next is
//! refused, because only the first page was established — a straddling read
//! takes its tail out of whatever the neighbouring frame happens to hold, and a
//! futex word is dereferenced on every wake check.
//!
//! The bound is also the one [`crate::fault`] classifies a trap against — one
//! constant, read by the check before an access and by the verdict after it.

/// One past the highest address userland can name.
///
/// The hardware's canonical split with 48-bit linear addresses: PML4 indices
/// 0..255 are the user half and 256..511 are the kernel's. An address at or
/// above this is either a kernel address or non-canonical, and neither is
/// something a process may hand the kernel to dereference on its behalf.
pub const USER_TOP: u64 = 0x0000_8000_0000_0000;

/// The kernel's only user page size, which is also the granularity a
/// translation answers at.
pub const PAGE_2M: u64 = 2 * 1024 * 1024;

/// Rounds `size` up to the next 2 MiB page, or `None` when the sum wraps
/// rather than silently rounding to an undersized allocation: for a size that
/// crossed a trust boundary, not one the kernel computed itself.
pub const fn align_2m_checked(size: u64) -> Option<u64> {
    match size.checked_add(PAGE_2M - 1) {
        Some(sum) => Some(sum & !(PAGE_2M - 1)),
        None => None,
    }
}

pub fn is_user_addr(addr: u64) -> bool {
    addr < USER_TOP
}

/// An instruction pointer the kernel may return to userland at.
///
/// A return to an address that is not canonical faults in the returning
/// instruction itself, in the kernel's ring and not the thread's, so one that
/// crossed the trust boundary is refused before any frame carries it. No
/// other constructor: a first return to userland takes this and nothing else.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry(u64);

impl Entry {
    /// `None` for an address outside the user half; what is mapped at one
    /// inside it is the thread's own fault to take.
    pub fn new(addr: u64) -> Option<Self> {
        is_user_addr(addr).then_some(Self(addr))
    }

    pub const fn addr(self) -> u64 {
        self.0
    }
}

/// One past the highest address the kernel places anything at, an image or a
/// [`Window`](crate::Window)'s range: the user half less its last page. An
/// instruction that ends at [`USER_TOP`] leaves that address, no canonical
/// one, as where a syscall or an interrupt taken after it returns to, so
/// nothing executable is placed where one could end there.
pub(crate) const PLACED_TOP: u64 = USER_TOP - PAGE_2M;

/// Whether `[ptr, ptr + len)` is entirely in the user half.
///
/// Also the bound `sys_mmap` applies to a range it will install rather than
/// dereference: the hardware split is the same either way, and a second copy of
/// the constant is a second thing to get wrong.
pub fn in_user_half(ptr: u64, len: u64) -> bool {
    match ptr.checked_add(len) {
        Some(end) => end <= USER_TOP,
        None => false,
    }
}

/// The load base an `ET_DYN` image rebases to `vm_base` (`vm_base - vaddr_min`),
/// or `None` when it cannot: a `vaddr_min` above `vm_base` underflows the
/// subtraction, and a `span` reaching from `vm_base` past [`PLACED_TOP`] does not
/// fit. The ELF spec leaves an `ET_DYN` `p_vaddr` unconstrained, so the kernel
/// that picks `vm_base` is the only place that can refuse one.
pub fn rebase_base(vm_base: u64, vaddr_min: u64, span: u64) -> Option<u64> {
    let base = vm_base.checked_sub(vaddr_min)?;
    let end = vm_base.checked_add(span)?;
    (end <= PLACED_TOP).then_some(base)
}

/// Whether the kernel may read or write a `size`-byte value of alignment
/// `align` at `ptr` through one translation.
///
/// A translation answers for the 2 MiB page holding its first byte; the
/// physical page after that one belongs to whoever the PMM last gave it to. So
/// an object crossing the boundary is refused rather than served out of the
/// page it started in — every `UserSafe` type is small enough for userland to
/// move, and the alternative is a write into another process's memory.
pub fn is_user_object(ptr: u64, size: u64, align: u64) -> bool {
    if size == 0 || !align.is_power_of_two() {
        return false;
    }
    if !ptr.is_multiple_of(align) || !in_user_half(ptr, size) {
        return false;
    }
    ptr & !(PAGE_2M - 1) == (ptr + size - 1) & !(PAGE_2M - 1)
}

/// What the kernel does through a user address, asked as the MMU asks the same
/// access from ring 3: a copy into user memory needs a page the process could
/// store to itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Access {
    Read,
    Write,
}

/// The grain a split window grants rights at.
pub const PAGE_4K: u64 = 4096;

#[cfg(test)]
mod tests {
    use super::*;

    /// Every size and alignment a `repr(C)` value of integers up to 256 bytes
    /// can have: the two numbers are all [`is_user_object`] reads of a type.
    fn shapes() -> impl Iterator<Item = (u64, u64)> {
        [1, 2, 4, 8].into_iter().flat_map(|align| (1..=256 / align).map(move |n| (n * align, align)))
    }

    #[test]
    fn a_kernel_address_is_not_a_user_address() {
        assert!(!is_user_addr(USER_TOP));
        assert!(!is_user_addr(0xFFFF_8000_0000_0000));
        assert!(!is_user_addr(u64::MAX));
        assert!(is_user_addr(USER_TOP - 1));
        assert!(is_user_addr(0));
    }

    /// The last address of the user half is one, and the first that is not
    /// canonical, the first of the kernel half and the last of all are not.
    #[test]
    fn an_entry_is_an_address_of_the_user_half_and_nothing_else() {
        assert_eq!(Entry::new(USER_TOP - 1).map(Entry::addr), Some(USER_TOP - 1));
        assert_eq!(Entry::new(0).map(Entry::addr), Some(0));
        assert_eq!(Entry::new(USER_TOP), None);
        assert_eq!(Entry::new(0x0100_0000_0000_0000), None);
        assert_eq!(Entry::new(0xFFFF_7FFF_FFFF_FFFF), None);
        assert_eq!(Entry::new(0xFFFF_8000_0000_0000), None);
        assert_eq!(Entry::new(u64::MAX), None);
    }

    #[test]
    fn a_range_ending_past_the_bound_is_refused_and_a_wrapping_one_too() {
        assert!(in_user_half(USER_TOP - 8, 8));
        assert!(!in_user_half(USER_TOP - 8, 9));
        assert!(!in_user_half(USER_TOP, 1));
        assert!(!in_user_half(u64::MAX, 1));
        assert!(!in_user_half(u64::MAX - 4, 8));
        assert!(!in_user_half(1 << 63, 1 << 63));
    }

    /// The straddle, for every type, at every offset that can produce one.
    #[test]
    fn no_type_may_cross_a_2_mib_boundary() {
        for (size, align) in shapes() {
            for last in 1..size {
                let ptr = 4 * PAGE_2M - last;
                if !ptr.is_multiple_of(align) {
                    continue;
                }
                assert!(
                    !is_user_object(ptr, size, align),
                    "{size}/{align} at {ptr:#x} has {last} bytes below the boundary"
                );
            }
            assert!(is_user_object(4 * PAGE_2M - size, size, align), "{size}/{align} ending at a boundary");
            assert!(is_user_object(4 * PAGE_2M, size, align), "{size}/{align} starting at a boundary");
        }
    }

    #[test]
    fn an_object_is_refused_for_its_alignment_before_anything_else() {
        for (size, align) in shapes() {
            for off in 1..align {
                assert!(!is_user_object(PAGE_2M + off, size, align), "{size}/{align} at +{off}");
            }
            assert!(is_user_object(PAGE_2M, size, align), "{size}/{align} at a page start");
        }
    }

    #[test]
    fn an_object_may_not_end_past_the_bound() {
        for (size, align) in shapes() {
            assert!(is_user_object(USER_TOP - size, size, align), "{size}/{align} ending at the bound");
            assert!(!is_user_object(USER_TOP, size, align), "{size}/{align} at the bound");
            assert!(!is_user_object(USER_TOP + PAGE_2M, size, align), "{size}/{align} above the bound");
            assert!(!is_user_object(u64::MAX - size + 1, size, align), "{size}/{align} wrapping");
        }
    }

    #[test]
    fn a_zero_sized_object_is_nothing_to_dereference() {
        assert!(!is_user_object(PAGE_2M, 0, 1));
        assert!(in_user_half(PAGE_2M, 0));
    }

    /// The loader's `USER_VM_BASE`, by value.
    const USER_VM_BASE: u64 = 0x100_0000_0000;

    #[test]
    fn an_image_at_vaddr_zero_rebases_to_the_base_itself() {
        assert_eq!(rebase_base(USER_VM_BASE, 0, PAGE_2M), Some(USER_VM_BASE));
        assert_eq!(rebase_base(USER_VM_BASE, USER_VM_BASE, PAGE_2M), Some(0));
    }

    /// The ELF spec fixes no ceiling on an `ET_DYN` `p_vaddr`, so `vaddr_min` may
    /// exceed the base — the subtraction the loader would underflow.
    #[test]
    fn a_vaddr_min_above_the_base_cannot_rebase() {
        assert_eq!(rebase_base(USER_VM_BASE, 0x200_0000_0000, PAGE_2M), None);
        assert_eq!(rebase_base(USER_VM_BASE, USER_VM_BASE + 1, PAGE_2M), None);
        assert_eq!(rebase_base(USER_VM_BASE, u64::MAX, PAGE_2M), None);
    }

    #[test]
    fn an_image_that_reaches_the_last_page_of_the_user_half_is_refused() {
        assert_eq!(rebase_base(USER_VM_BASE, 0, PLACED_TOP - USER_VM_BASE), Some(USER_VM_BASE));
        assert_eq!(rebase_base(USER_VM_BASE, 0, PLACED_TOP - USER_VM_BASE + 1), None);
        assert_eq!(rebase_base(USER_VM_BASE, 0, USER_TOP - USER_VM_BASE), None);
        assert_eq!(rebase_base(USER_VM_BASE, 0, u64::MAX), None);
        assert_eq!(rebase_base(u64::MAX, 0, 1), None);
    }
}
