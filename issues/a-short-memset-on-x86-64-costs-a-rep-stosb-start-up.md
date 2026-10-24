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

**Exit.** `fill` takes a short-length path, or selects by the fast short `rep stosb` feature where the CPU has it. `memcpy`'s `rep movsb` may share the cost; it is unmeasured. The T14 reads a 0 B and a 64 B `memset` within a few nanoseconds of a store loop's. The kernel's counterpart is `issues/kernel-forward-copies-and-fills-are-one-rep-movsb-or-stosb-on-every-cpu.md`.
