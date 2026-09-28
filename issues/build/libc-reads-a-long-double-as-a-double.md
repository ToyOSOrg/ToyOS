---
status: open
kind: defect
opened: 2026-09-27
---

# libc reads a `long double` argument as a `double`

`userland/libc/src/printf.rs` takes `%Lf`'s `L` and then reads a `double`
(`let _ = long_double; // our long double == double`). On x86-64 a C `long
double` is the 80-bit x87 type, 16 bytes, passed in memory; a `double` is read
out of the SSE registers. So `printf("%Lf", x)` prints whatever the registers
hold — 0.000000 where the call's `double`s filled them.

toyos-cc compiled `long double` as `double`, so the two agreed and TinyCC's
`22_floating_point` passed. clang compiles the ABI the platform has, and the
case is declined in `tests/toyos.rs`'s `NOT_RUN` with this entry named, as are
`73_arm64` and `101_cleanup` in part. Nothing ToyOS builds prints a `long
double`; doomgeneric has none.

**Exit**: libc's `printf` family reads `%Lf`, `%Le` and `%Lg` as the 80-bit type
(and `strtold` returns one), and `22_floating_point` runs again.
