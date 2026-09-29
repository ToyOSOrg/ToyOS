---
status: open
kind: track
opened: 2026-09-29
---

# Programs hold AES keys as Key Locker handles

Under Key Locker a program wraps an AES key into a handle under a wrapping key
the kernel loads into every CPU, and encrypts with the handle, so the key
itself need not stay in memory. Linux at `Ubuntu-6.8.0-142.142` has none:
nothing under its `arch/x86` names Key Locker. It is blocked on its first
consumer, a program that keeps an AES key for a session's life, and lands with
it, after `issues/kernel/no-test-can-hold-a-thread-on-a-named-cpu.md`.

**Exit**: on each proving machine that enumerates CPUID.19H:EBX bits 0 and 4
and ECX bit 1, a handle wrapped on one pinned CPU gives FIPS-197 C.1's
ciphertext on another. **Mutation**: a wrapping key per CPU. **Oracle**:
FIPS-197.
