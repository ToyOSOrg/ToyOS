---
status: owner
kind: question
opened: 2026-10-09
---

# x86-64 hashing runs scalar where the CPU has SHA instructions

`toyos-sha2` is scalar on every target and forbids `unsafe`, so the callers
that took `sha2`'s x86 backend — SHA-NI for SHA-256 and AVX2 for SHA-512,
chosen by CPUID — lost it: `update`'s streamed ROOT hash, `pkg` and `swap` on
the T14, and the build's hashing (`src/cicache.rs`, `src/image.rs`'s
`root_uuid`, `src/sysroot.rs`) on x86-64 CI runners. The loader is not among
them: its target is soft-float and it was scalar before.

The loss is bounded by the scalar time itself, which the T14 measured: the
loader on main at d6298c83e hashed the `testcases` image's 320,864,256-byte
ROOT with `sha2`'s soft backend in 5,343,577,520 counter ticks at 2,419,200,000
Hz, 2.21 s (138.5 MiB/s), and read the same ROOT off the stick in 9.31 s. So
`update` gives up at most 2.2 s of a 306 MiB ROOT it also writes, and at most
0.4 s of the default image's 56 MiB one; the build's largest hash, every
tracked file for `cicache` (47,906,184 bytes at c8aad54b1), at most 0.33 s
at that rate. `update`'s own hash time on the T14 is unmeasured.

An instruction path needs `core::arch` intrinsics, which are `unsafe`, in an
x86-64 module of the crate with a CPUID selector, for userland callers alone.

The question: is up to 2.2 s of an update worth `unsafe` in the crate every
signed image is verified with?

Exit: the owner's ruling; on a yes, the T14's `update` reading of its ROOT hash
before and after the instruction path lands.
