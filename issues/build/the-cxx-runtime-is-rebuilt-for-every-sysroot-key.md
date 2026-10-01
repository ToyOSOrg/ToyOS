---
status: open
kind: tooling
opened: 2026-10-01
---

# The C++ runtime is rebuilt for every sysroot key

`sysroot::build` builds libc++, libc++abi and libunwind for both userland
targets into every new sysroot (`libcxx::build`), so every `toyos-abi` edit
pays for them. Their inputs are the compiler's LLVM, `libcxx::OPTIONS` and the
C sysroot they compile and link against: libc's headers and its staticlib. An
ABI edit moves only the staticlib, and an edit that leaves its bytes the same
moves none of them. Measured on the `wt/toyos-rebuild` branch, one item added
to `toyos-abi/src/clock.rs`: 27 s and 38 s at load 41, 47 s and 54 s at load 68.

**Exit**: the C++ runtime is keyed by what it is built from, the C sysroot's
bytes among them, and an ABI edit that leaves those bytes unchanged builds no
C++ runtime.
