//! What osbench needs written for AArch64 in particular, on Linux: a chain of
//! dependent adds to time the clock with, the bytes of a program that exits,
//! and no power registers. ToyOS on AArch64 is refused by the selector.

/// The energy group reads no AArch64 power counter.
pub fn power_registers() -> Option<crate::sys::PowerRegisters> {
    None
}

/// `turns` turns of 100 dependent register adds, each waiting on the one
/// before it. Inline assembly, so the chain is the one written whatever the
/// optimizer does.
pub fn add_chain(turns: u64) {
    let (mut x, mut n) = (std::hint::black_box(0u64), turns);
    // SAFETY: arithmetic on two registers this block owns.
    unsafe {
        std::arch::asm!(
            "2:",
            ".rept 100",
            "add {x}, {x}, #1",
            ".endr",
            "subs {n}, {n}, #1",
            "b.ne 2b",
            x = inout(reg) x,
            n = inout(reg) n,
            options(nomem, nostack),
        );
    }
    std::hint::black_box((x, n));
}

/// Adds per turn of [`add_chain`].
pub const ADDS_PER_TURN: u64 = 100;

/// The ELF machine a generated program is for (`EM_AARCH64`).
pub const ELF_MACHINE: u16 = 183;

/// What a generated program runs, position-independent: `exit_group(0)`
/// (AArch64's syscall 94, the number in X8 and the code in X0), then a
/// permanently undefined instruction that is never reached.
pub const EXIT_CODE: [u8; 16] = [
    0xc8, 0x0b, 0x80, 0xd2, // movz x8, #94 (exit_group)
    0x00, 0x00, 0x80, 0xd2, // movz x0, #0
    0x01, 0x00, 0x00, 0xd4, // svc #0
    0x00, 0x00, 0x00, 0x00, // udf #0
];

const _: () = assert!(libc::SYS_exit_group == 94);

/// AArch64 names a CPU to EL0 by no register: `MPIDR_EL1` is EL1's.
pub fn cpu_id() -> Result<u32, &'static str> {
    Err("AArch64 has no register naming the CPU that EL0 can read")
}

pub fn cpu_id_is() -> &'static str {
    "none"
}
