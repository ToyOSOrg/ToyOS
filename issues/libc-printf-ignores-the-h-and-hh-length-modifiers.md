---
status: open
kind: defect
opened: 2026-10-15
---

# libc's printf ignores the `h` and `hh` length modifiers

`sdk/libc/src/conversion.rs` steps over `h` and `hh`, and `sdk/libc/src/printf.rs`
prints the promoted `int` whole: `printf("%hhd", 300)` prints `300` where C17
§7.21.6.1p7 converts the value to `signed char` first and prints `44`, and
`%hx` of `0x12345` prints `12345` where C has `2345`.

**Exit**: `h` and `hh` narrow `d`, `i`, `o`, `u`, `x` and `X` to `short` and
`char`, with host cases in `sdk/libc/arch/src/printf_conversions.rs`.
