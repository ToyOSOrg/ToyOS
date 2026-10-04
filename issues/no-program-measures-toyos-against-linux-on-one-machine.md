---
status: open
kind: tooling
opened: 2026-09-29
---

# No program measures ToyOS against Linux on one machine

One Rust program, built for `x86_64-unknown-linux-gnu` and for ToyOS, is to
measure memcpy from 64 B to 64 MiB; AES-128-GCM, ChaCha20-Poly1305 and SHA-256
at sshd's RustCrypto versions; pipe round trips, and throughput at 64 B to 64
KiB; munmaps per second of a page a sibling on another CPU touched; lateness
past a 1 ms timer; NVMe reads, read-only; an idle minute's package energy; and
the T14's PCI, USB, NVMe and cpuidle identity. It runs on the T14 under its
Ubuntu before the wipe and under the shipped ToyOS kernel. A munmap pair's two
sides each read their x2APIC ID from CPUID.0BH:EDX before and after the timed
window, and a pure function over the four readings refuses the figure when the
two sides' IDs match or either side's changed across the window.

**Exit**: the outputs committed as fixtures, and that function host-tested.
**Mutation**, each red: a pair on one CPU passes; only the readings before the
window compared, so a sibling that moved onto the initiator's CPU inside it
passes. **Oracle**: Linux.
