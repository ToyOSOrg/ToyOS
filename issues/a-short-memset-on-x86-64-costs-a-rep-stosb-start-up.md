---
status: open
kind: defect
opened: 2026-10-24
---

# A short `memset` on x86-64 ToyOS costs a `rep stosb` start-up

`sdk/libc`'s `memset` (`sdk/libc/src/memory.rs`) is `arch::fill`, one unconditional
`rep stosb` at every length (`sdk/libc/src/arch/x86_64.rs`), and Rust's own `memset`
on ToyOS resolves to it. The T14 (Tiger Lake) has no fast short `rep stosb`
(`issues/features-no-proving-machine-is-known-to-offer.md`), so every short fill
any program makes pays the instruction's start-up.

**Evidence.** `cryptocase` on the T14 at #1054's 43690c542.
- AES-GCM's refusal zeroed the record with `fill(0)`. reject/0 read 107.6–108.7 ns, against 56.7–58.8 ns on main in every recent readback.
- At 0 B that refusal does a zero-length `memset` and little else.
- The Linux probe, on musl's `memset`, read 51 ns at both revisions.
- With #1054's 16-byte volatile stores in its place, reject/0 read 56.7 and 61.2 ns, main's level. The `memset` was the whole difference: about 50 ns for a zero-length call.
- The ToyOS-target disassembly of that build's open calls no `memset`.
- Evidence: https://github.com/ToyOSOrg/evidence/tree/db8683ad53396c00cef576007349d3662d4c7d78/r4

**Owner:** `sdk/libc`. `toyos-crypto` works around the cost: `block.rs`'s header, and `secret::wipe` at the refusal.
- That wipe costs more than `rep stosb` on long records. reject at 16 KiB reads 0.98 against ring on the T14 (0.76 with the `memset`) and 1.11–1.14 on the AMD bench (1.03).
- Evidence for those readings: the r4 judge logs linked above.

**Exit.** `fill` takes a short-length path, or selects by the fast short `rep stosb` feature where the CPU has it. The crate's workarounds then go: the refusal returns to `fill(0)`, and `block.rs`'s header drops its reason. `memcpy`'s `rep movsb` may share it: `mem.memcpy.64` reads 0.61 of Linux's on the AMD bench (`issues/a-small-memcpy-on-the-amd-bench-runs-at-as-little-as-six-tenths-of-linuxs.md`). The T14 reads a 0 B and a 64 B `memset` within a few nanoseconds of a store loop's. The kernel's counterpart is `issues/kernel-forward-copies-and-fills-are-one-rep-movsb-or-stosb-on-every-cpu.md`.
