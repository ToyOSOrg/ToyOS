//! Copies between kernel memory and the running process's user half, through
//! that process's own mapping (`crate::user_ptr`'s contract).
//!
//! Each user access is an unprivileged `LDTR`/`STTR`, checked at EL0's
//! permissions whatever `PSTATE.PAN` says (Arm ARM K.a, C6.2: "Load/Store
//! Register (unprivileged)"), so a copy needs no PAN toggle and reaches only
//! what the process could. The bulk moves 32 bytes a step, four 8-byte user
//! accesses beside two paired kernel-side ones, then 8 bytes, then single
//! bytes. The count left is lowered only once a step's accesses are done, so
//! a fault anywhere in a routine leaves `x2` counting every byte not yet
//! written: a fault the abort path serves resumes at the faulting access, and
//! one it cannot serve resumes at the routine's fixup, which answers `x2`.
//! [`fixup`] is the exception table: only these routines' bodies may take an
//! EL1 abort on a user address and live.
//!
//! The user address is checked against the half's top by the caller; each
//! routine also replaces an address at or past it with 0 by `csel` and a
//! `csdb`, so a mispredicted check reads no kernel byte even speculatively
//! (`issues/user-pointer-checks-have-no-spectre-v1-fence.md`).
//! A copy through a user mapping of device memory makes only unprivileged
//! accesses the process could make through that same mapping itself; an
//! unaligned one faults, and the copy ends there.

use toyos_userbound::USER_TOP;

use crate::user_ptr::Faulted;

core::arch::global_asm!(
    ".pushsection .text.toyos_uaccess, \"ax\"",
    // toyos_copy_from_user(x0 = kernel dst, x1 = user src, x2 = len) -> x0 = bytes not copied
    ".balign 16",
    ".global toyos_copy_from_user",
    "toyos_copy_from_user:",
    "mov x9, #{top_hi}",
    "lsl x9, x9, #32",
    "cmp x1, x9",
    "csel x1, xzr, x1, hs",
    "hint #20", // csdb
    ".global toyos_copy_from_user_begin",
    "toyos_copy_from_user_begin:",
    "1:",
    "cmp x2, #32",
    "b.lo 2f",
    "ldtr x3, [x1]",
    "ldtr x4, [x1, #8]",
    "ldtr x5, [x1, #16]",
    "ldtr x6, [x1, #24]",
    "stp x3, x4, [x0]",
    "stp x5, x6, [x0, #16]",
    "add x0, x0, #32",
    "add x1, x1, #32",
    "sub x2, x2, #32",
    "b 1b",
    "2:",
    "cmp x2, #8",
    "b.lo 3f",
    "ldtr x3, [x1]",
    "str x3, [x0], #8",
    "add x1, x1, #8",
    "sub x2, x2, #8",
    "b 2b",
    "3:",
    "cbz x2, 4f",
    "ldtrb w3, [x1]",
    "strb w3, [x0], #1",
    "add x1, x1, #1",
    "sub x2, x2, #1",
    "b 3b",
    ".global toyos_copy_from_user_end",
    "toyos_copy_from_user_end:",
    "4:",
    "mov x0, x2",
    "ret",
    // toyos_copy_to_user(x0 = user dst, x1 = kernel src, x2 = len) -> x0 = bytes not copied
    ".balign 16",
    ".global toyos_copy_to_user",
    "toyos_copy_to_user:",
    "mov x9, #{top_hi}",
    "lsl x9, x9, #32",
    "cmp x0, x9",
    "csel x0, xzr, x0, hs",
    "hint #20", // csdb
    ".global toyos_copy_to_user_begin",
    "toyos_copy_to_user_begin:",
    "1:",
    "cmp x2, #32",
    "b.lo 2f",
    "ldp x3, x4, [x1]",
    "ldp x5, x6, [x1, #16]",
    "sttr x3, [x0]",
    "sttr x4, [x0, #8]",
    "sttr x5, [x0, #16]",
    "sttr x6, [x0, #24]",
    "add x0, x0, #32",
    "add x1, x1, #32",
    "sub x2, x2, #32",
    "b 1b",
    "2:",
    "cmp x2, #8",
    "b.lo 3f",
    "ldr x3, [x1], #8",
    "sttr x3, [x0]",
    "add x0, x0, #8",
    "sub x2, x2, #8",
    "b 2b",
    "3:",
    "cbz x2, 4f",
    "ldrb w3, [x1], #1",
    "sttrb w3, [x0]",
    "add x0, x0, #1",
    "sub x2, x2, #1",
    "b 3b",
    ".global toyos_copy_to_user_end",
    "toyos_copy_to_user_end:",
    "4:",
    "mov x0, x2",
    "ret",
    // toyos_clear_user(x0 = user dst, x1 = len) -> x0 = bytes not cleared
    ".balign 16",
    ".global toyos_clear_user",
    "toyos_clear_user:",
    "mov x9, #{top_hi}",
    "lsl x9, x9, #32",
    "cmp x0, x9",
    "csel x0, xzr, x0, hs",
    "hint #20", // csdb
    "mov x2, x1",
    ".global toyos_clear_user_begin",
    "toyos_clear_user_begin:",
    "1:",
    "cmp x2, #8",
    "b.lo 3f",
    "sttr xzr, [x0]",
    "add x0, x0, #8",
    "sub x2, x2, #8",
    "b 1b",
    "3:",
    "cbz x2, 4f",
    "sttrb wzr, [x0]",
    "add x0, x0, #1",
    "sub x2, x2, #1",
    "b 3b",
    ".global toyos_clear_user_end",
    "toyos_clear_user_end:",
    "4:",
    "mov x0, x2",
    "ret",
    ".popsection",
    top_hi = const USER_TOP >> 32,
);

// The top is a whole multiple of 2^32, so one `mov` and a shift build it.
const _: () = assert!(USER_TOP & 0xFFFF_FFFF == 0 && USER_TOP >> 32 <= 0xFFFF);

unsafe extern "C" {
    fn toyos_copy_from_user(dst: *mut u8, user: u64, len: usize) -> usize;
    fn toyos_copy_to_user(user: u64, src: *const u8, len: usize) -> usize;
    fn toyos_clear_user(user: u64, len: usize) -> usize;
    static toyos_copy_from_user_begin: u8;
    static toyos_copy_from_user_end: u8;
    static toyos_copy_to_user_begin: u8;
    static toyos_copy_to_user_end: u8;
    static toyos_clear_user_begin: u8;
    static toyos_clear_user_end: u8;
}

/// A routine's answer: 0 where it copied every byte.
fn whole(left: usize) -> Result<(), Faulted> {
    if left == 0 { Ok(()) } else { Err(Faulted) }
}

/// Copy `len` bytes from user address `user` to `dst`. On `Err` some or none
/// of them were copied.
///
/// # Safety
/// `dst` is writable kernel memory for `len` bytes; `[user, user + len)` lies
/// below [`USER_TOP`]; no lock is held (`crate::user_ptr`).
pub unsafe fn copy_from_user(dst: *mut u8, user: u64, len: usize) -> Result<(), Faulted> {
    // SAFETY: the caller's contract; the routine touches nothing else.
    whole(unsafe { toyos_copy_from_user(dst, user, len) })
}

/// Copy `len` bytes from `src` to user address `user`, as [`copy_from_user`].
///
/// # Safety
/// As [`copy_from_user`], with `src` readable.
pub unsafe fn copy_to_user(user: u64, src: *const u8, len: usize) -> Result<(), Faulted> {
    // SAFETY: the caller's contract; the routine touches nothing else.
    whole(unsafe { toyos_copy_to_user(user, src, len) })
}

/// Zero `len` bytes at user address `user`, as [`copy_from_user`].
///
/// # Safety
/// As [`copy_from_user`].
pub unsafe fn clear_user(user: u64, len: usize) -> Result<(), Faulted> {
    // SAFETY: the caller's contract; the routine touches nothing else.
    whole(unsafe { toyos_clear_user(user, len) })
}

/// Where an EL1 abort at `pc` resumes when it cannot be served: `Some` only
/// inside one of the routines above, at its fixup.
pub fn fixup(pc: u64) -> Option<u64> {
    let bodies = [
        (&raw const toyos_copy_from_user_begin as u64, &raw const toyos_copy_from_user_end as u64),
        (&raw const toyos_copy_to_user_begin as u64, &raw const toyos_copy_to_user_end as u64),
        (&raw const toyos_clear_user_begin as u64, &raw const toyos_clear_user_end as u64),
    ];
    bodies.iter().find(|&&(begin, end)| (begin..end).contains(&pc)).map(|&(_, end)| end)
}
