//! Copies between kernel memory and the running process's user half, through
//! that process's own mapping (`crate::user_ptr`'s contract).
//!
//! Each routine opens SMAP's door with `stac` around its copy and closes it
//! with `clac`. A copy goes as the kernel's `memcpy` does, by its rule
//! (`super::copy`): one `rep movsb`, or the descending copy where a forward
//! one would stall. "A repeating string operation can be suspended by an
//! exception or interrupt. When this happens, the state of the registers is
//! preserved to allow the string operation to be resumed" (Intel SDM Vol. 2B,
//! REP/REPE/REPZ/REPNE/REPNZ), and a faulting `mov` is run again. So a fault
//! the page-fault path serves resumes the copy where it stopped, and one it
//! cannot serve resumes at the copy's fixup, which closes the door and
//! answers that the copy failed. [`fixup`] is the exception table: only these
//! copies' instructions may take a Ring 0 fault on a user address and live.
//!
//! The user address is checked against the half's top by the caller; each
//! routine also replaces an address at or past it with 0 by `cmovae`, without
//! a branch, so a mispredicted check reads no kernel byte even speculatively
//! (`issues/user-pointer-checks-have-no-spectre-v1-fence.md`).
//! A copy through a user mapping of device memory makes only the accesses the
//! process could make through that same mapping with the same instruction.

use toyos_userbound::USER_TOP;

use super::copy::{self, copy_down_asm};
use crate::user_ptr::Stopped;

core::arch::global_asm!(
    ".pushsection .text.toyos_uaccess, \"ax\"",
    // toyos_copy_from_user(rdi = kernel dst, rsi = user src, rdx = len) -> rax = 0 where it copied them all
    ".balign 16",
    ".global toyos_copy_from_user",
    "toyos_copy_from_user:",
    "xor ecx, ecx",
    "mov rax, {top}",
    "cmp rsi, rax",
    "cmovae rsi, rcx",
    "mov rcx, rdx",
    "stac",
    ".global toyos_copy_from_user_access",
    "toyos_copy_from_user_access:",
    "rep movsb",
    "clac",
    "mov rax, rcx",
    "ret",
    // toyos_copy_to_user(rdi = user dst, rsi = kernel src, rdx = len) -> rax = 0 where it copied them all
    ".balign 16",
    ".global toyos_copy_to_user",
    "toyos_copy_to_user:",
    "xor ecx, ecx",
    "mov rax, {top}",
    "cmp rdi, rax",
    "cmovae rdi, rcx",
    "mov rcx, rdx",
    "stac",
    ".global toyos_copy_to_user_access",
    "toyos_copy_to_user_access:",
    "rep movsb",
    "clac",
    "mov rax, rcx",
    "ret",
    // The same two highest first, for a length of at least 8. The copy uses
    // no stack, so its fixup returns from wherever it stopped.
    ".balign 16",
    ".global toyos_copy_from_user_down",
    "toyos_copy_from_user_down:",
    "xor ecx, ecx",
    "mov rax, {top}",
    "cmp rsi, rax",
    "cmovae rsi, rcx",
    "stac",
    ".global toyos_copy_from_user_down_access",
    "toyos_copy_from_user_down_access:",
    copy_down_asm!(),
    ".global toyos_copy_from_user_down_end",
    "toyos_copy_from_user_down_end:",
    "clac",
    "xor eax, eax",
    "ret",
    ".balign 16",
    ".global toyos_copy_to_user_down",
    "toyos_copy_to_user_down:",
    "xor ecx, ecx",
    "mov rax, {top}",
    "cmp rdi, rax",
    "cmovae rdi, rcx",
    "stac",
    ".global toyos_copy_to_user_down_access",
    "toyos_copy_to_user_down_access:",
    copy_down_asm!(),
    ".global toyos_copy_to_user_down_end",
    "toyos_copy_to_user_down_end:",
    "clac",
    "xor eax, eax",
    "ret",
    ".global toyos_copy_down_fault",
    "toyos_copy_down_fault:",
    "clac",
    "mov eax, 1",
    "ret",
    // toyos_clear_user(rdi = user dst, rsi = len) -> rax = 0 where it cleared them all
    ".balign 16",
    ".global toyos_clear_user",
    "toyos_clear_user:",
    "xor ecx, ecx",
    "mov rax, {top}",
    "cmp rdi, rax",
    "cmovae rdi, rcx",
    "mov rcx, rsi",
    "xor eax, eax",
    "stac",
    ".global toyos_clear_user_access",
    "toyos_clear_user_access:",
    "rep stosb",
    "clac",
    "mov rax, rcx",
    "ret",
    ".popsection",
    top = const USER_TOP,
);

unsafe extern "C" {
    fn toyos_copy_from_user(dst: *mut u8, user: u64, len: usize) -> usize;
    fn toyos_copy_to_user(user: u64, src: *const u8, len: usize) -> usize;
    fn toyos_copy_from_user_down(dst: *mut u8, user: u64, len: usize) -> usize;
    fn toyos_copy_to_user_down(user: u64, src: *const u8, len: usize) -> usize;
    fn toyos_clear_user(user: u64, len: usize) -> usize;
    static toyos_copy_from_user_access: u8;
    static toyos_copy_to_user_access: u8;
    static toyos_clear_user_access: u8;
    static toyos_copy_from_user_down_access: u8;
    static toyos_copy_from_user_down_end: u8;
    static toyos_copy_to_user_down_access: u8;
    static toyos_copy_to_user_down_end: u8;
    static toyos_copy_down_fault: u8;
}

/// Bytes a string instruction takes: its `rep` prefix and its opcode.
const REP_STRING_LEN: u64 = 2;

/// A routine's answer: 0 where it copied every byte.
fn whole(left: usize) -> Result<(), Stopped> {
    if left == 0 { Ok(()) } else { Err(crate::user_ptr::last_stop()) }
}

/// Copy `len` bytes from user address `user` to `dst`. On `Err` some or none
/// of them were copied.
///
/// # Safety
/// `dst` is writable kernel memory for `len` bytes; `[user, user + len)` lies
/// below [`USER_TOP`]; no lock is held (`crate::user_ptr`).
pub unsafe fn copy_from_user(dst: *mut u8, user: u64, len: usize) -> Result<(), Stopped> {
    // SAFETY: the caller's contract; each routine touches nothing else, and
    // the rule sends down only a copy of at least 8 bytes.
    whole(unsafe {
        match copy::rule().goes_down(dst as usize, user as usize, len) {
            true => toyos_copy_from_user_down(dst, user, len),
            false => toyos_copy_from_user(dst, user, len),
        }
    })
}

/// Copy `len` bytes from `src` to user address `user`, as [`copy_from_user`].
///
/// # Safety
/// As [`copy_from_user`], with `src` readable.
pub unsafe fn copy_to_user(user: u64, src: *const u8, len: usize) -> Result<(), Stopped> {
    // SAFETY: as in `copy_from_user`.
    whole(unsafe {
        match copy::rule().goes_down(user as usize, src as usize, len) {
            true => toyos_copy_to_user_down(user, src, len),
            false => toyos_copy_to_user(user, src, len),
        }
    })
}

/// Zero `len` bytes at user address `user`, as [`copy_from_user`].
///
/// # Safety
/// As [`copy_from_user`].
pub unsafe fn clear_user(user: u64, len: usize) -> Result<(), Stopped> {
    // SAFETY: the caller's contract; the routine touches nothing else.
    whole(unsafe { toyos_clear_user(user, len) })
}

/// Where a Ring 0 fault at `rip` resumes when it cannot be served: `Some` only
/// for the string instruction of one of the routines above, or an instruction
/// of a descending copy.
pub fn fixup(rip: u64) -> Option<u64> {
    let strings = [
        &raw const toyos_copy_from_user_access as u64,
        &raw const toyos_copy_to_user_access as u64,
        &raw const toyos_clear_user_access as u64,
    ];
    let descending = [
        (&raw const toyos_copy_from_user_down_access as u64)..(&raw const toyos_copy_from_user_down_end as u64),
        (&raw const toyos_copy_to_user_down_access as u64)..(&raw const toyos_copy_to_user_down_end as u64),
    ];
    if strings.contains(&rip) {
        Some(rip + REP_STRING_LEN)
    } else if descending.iter().any(|copy| copy.contains(&rip)) {
        Some(&raw const toyos_copy_down_fault as u64)
    } else {
        None
    }
}
