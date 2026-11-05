//! Copies between kernel memory and the running process's user half, through
//! that process's own mapping (`crate::user_ptr`'s contract).
//!
//! Each routine opens SMAP's door with `stac` around one `rep movsb` or
//! `rep stosb` and closes it with `clac`. "A repeating string operation can be
//! suspended by an exception or interrupt. When this happens, the state of the
//! registers is preserved to allow the string operation to be resumed" (Intel
//! SDM Vol. 2B, REP/REPE/REPZ/REPNE/REPNZ). So a fault the page-fault path
//! serves resumes the copy where it stopped, and one it cannot serve resumes
//! at the instruction after it, the fixup, which closes the door and answers
//! what `RCX` still counts. [`fixup`] is the exception
//! table: only these three instructions may take a Ring 0 fault on a user
//! address and live.
//!
//! The user address is checked against the half's top by the caller; each
//! routine also replaces an address at or past it with 0 by `cmovae`, without
//! a branch, so a mispredicted check reads no kernel byte even speculatively
//! (`issues/user-pointer-checks-have-no-spectre-v1-fence-and-smap-is-optional.md`).
//! A copy through a user mapping of device memory makes only the accesses the
//! process could make through that same mapping with the same instruction.

use toyos_userbound::USER_TOP;

core::arch::global_asm!(
    ".pushsection .text.toyos_uaccess, \"ax\"",
    // toyos_copy_from_user(rdi = kernel dst, rsi = user src, rdx = len) -> rax = bytes not copied
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
    // toyos_copy_to_user(rdi = user dst, rsi = kernel src, rdx = len) -> rax = bytes not copied
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
    // toyos_clear_user(rdi = user dst, rsi = len) -> rax = bytes not cleared
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
    fn toyos_clear_user(user: u64, len: usize) -> usize;
    static toyos_copy_from_user_access: u8;
    static toyos_copy_to_user_access: u8;
    static toyos_clear_user_access: u8;
}

/// Bytes a string instruction takes: its `rep` prefix and its opcode.
const REP_STRING_LEN: u64 = 2;

/// Copy `len` bytes from user address `user` to `dst`; answers the bytes not
/// copied.
///
/// # Safety
/// `dst` is writable kernel memory for `len` bytes; `[user, user + len)` lies
/// below [`USER_TOP`]; no lock is held (`crate::user_ptr`).
pub unsafe fn copy_from_user(dst: *mut u8, user: u64, len: usize) -> usize {
    // SAFETY: the caller's contract; the routine touches nothing else.
    unsafe { toyos_copy_from_user(dst, user, len) }
}

/// Copy `len` bytes from `src` to user address `user`; answers the bytes not
/// copied.
///
/// # Safety
/// As [`copy_from_user`], with `src` readable.
pub unsafe fn copy_to_user(user: u64, src: *const u8, len: usize) -> usize {
    // SAFETY: the caller's contract; the routine touches nothing else.
    unsafe { toyos_copy_to_user(user, src, len) }
}

/// Zero `len` bytes at user address `user`; answers the bytes not zeroed.
///
/// # Safety
/// As [`copy_from_user`].
pub unsafe fn clear_user(user: u64, len: usize) -> usize {
    // SAFETY: the caller's contract; the routine touches nothing else.
    unsafe { toyos_clear_user(user, len) }
}

/// Where a Ring 0 fault at `rip` resumes when it cannot be served: `Some` only
/// for the string instruction of one of the routines above.
pub fn fixup(rip: u64) -> Option<u64> {
    let accesses = [
        &raw const toyos_copy_from_user_access as u64,
        &raw const toyos_copy_to_user_access as u64,
        &raw const toyos_clear_user_access as u64,
    ];
    accesses.contains(&rip).then_some(rip + REP_STRING_LEN)
}
