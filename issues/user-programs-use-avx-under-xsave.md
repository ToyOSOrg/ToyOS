---
status: open
kind: track
opened: 2026-09-29
---

# User programs use AVX to AVX-512 under XSAVE

`CR4.OSXSAVE` stays clear and a thread's state is its `FXSAVE64` image
(`kernel/src/arch/x86_64/fpu.rs:6-7`), so AVX is `#UD` in every user program.
The kernel enables every vector component the CPU enumerates and saves them
by one path on every CPU, XSAVEOPT, refusing by name a CPU without it: TCG
implements neither XSAVEC nor XSAVES (`target/i386/cpu.c:1012-1014` at QEMU
v11.1.1), and the CET state of
`issues/user-programs-run-without-a-shadow-stack.md` is switched by
`wrmsr`, as `fs_base` is. The TCG model gains `+xsave,+xsaveopt,+avx,+avx2`,
which TCG implements (`cpu.c:905,907,981,1012`).

**Exit**: on every proving machine and under TCG, threads that fill their
vector registers differently each find their own after a switch; the T14's
ChaCha20-Poly1305 figure. **Mutation**: a save mask short of Hi16_ZMM, and
under TCG of YMM. **Oracle**: TCG and the T14 on SDM Vol. 1 §13.
