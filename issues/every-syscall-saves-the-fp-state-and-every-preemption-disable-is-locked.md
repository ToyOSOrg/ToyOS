---
status: open
kind: defect
opened: 2026-10-12
---

# Every syscall saves the FP state, and every preemption disable is a locked instruction

The syscall stub parks the user's whole FP/SSE image on the kernel stack with
`fxsave64` and restores it with `fxrstor64` on the way out
(`kernel/src/arch/x86_64/entry.rs:98-121`, `syscall.rs:61`), whether or not the
thread is switched out, though the kernel is built soft-float and never touches
those registers (`build/build.rs:511-522`). Saving at the ring transition is
what lets a switch skip it; every syscall that returns to its own thread pays
it anyway. The SDM has a preemptive OS save the state as part of the switch
and restore it when the task resumes (325462-093, Vol. 3A §16.4), and AMD's
APM names FXSAVE and FXRSTOR as what saves and restores it during a context
switch (24593 rev. 3.42, Vol. 2 §2.5.12). PR #859, open, makes the image an
XSAVE area: under XSAVES the T14 would park 2432 bytes, computed and not yet
read from a boot (that PR's body).

Every preemption disable on x86_64, `percpu::preempt_count_up` and
`preempt_count_down` (`kernel/src/arch/x86_64/percpu.rs:791-802`), is a
`lock add` or `lock sub` on the CPU's own count (`gs::lock_inc_u32` and
`lock_dec_u32`, `percpu.rs:299-315`). The word is private to the CPU, and the
reason given for the prefix, that an interrupt can split a plain `add`'s load
and store, is not the hardware's: an interrupt is taken only on an
instruction boundary (SDM 325462-093, Vol. 3A §7.6). The syscall stub's own
raise and lower around the handler (`syscall.rs:63`, `:67`) are the same instructions,
but they are `issues/syscall-preemption-is-incidental.md`'s: that file's step
of `issues/toyos-beats-linuxs-latency-on-the-t14.md` removes the raise, and
this file does not.

**Measured** (`wt/toyos-scout-sys` at `fbb958b17`, `main`'s `177e72a9e` plus
the scout, so the FXSAVE64 entry;
https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119438569). A null
syscall (`SYS_GETPID`, the minimum rep of 9×20000) in TSC cycles, with each
change made alone and both together:

| bench | base | no FP save | no `lock` | both | FP share | `lock` share | both, share |
|---|---|---|---|---|---|---|---|
| T14 (2419 MHz) | 646 | 560 | 499 | 423 | 13.3 % | 22.8 % | 34.5 % |
| AMD bench (1796 MHz) | 385 | 274 | 328 | 218 | 28.8 % | 14.8 % | 43.4 % |

The shares are (base − arm) / base, derived. The FP saving matches the pair's
own cost timed in Ring 3 (79 cycles on the T14, 111 on the AMD bench).

**Unexplained: where the `lock` saving comes from.** The no-`lock` arm
stripped the prefix from the stub's raise and lower and from
`gs::lock_inc_u32`/`lock_dec_u32`, so it covers both files' sites. Timed in
Ring 3, a prefix costs 7 cycles over a plain `add` on the T14 and 5 on the AMD
bench, which predicts about 15–30 and 10–12 cycles for the stub's two. The
arm saved 147 and 57. This run does not say which sites account for the
rest. The next measurement strips the prefix one site at a time, the stub's
pair apart from `preempt_count_up`/`_down`, and counts how many of the latter
a null syscall takes.

**Exit**: the `lock` saving split by site, posted; the save gone from a
syscall that returns to its own thread, and the `lock` prefix gone from
`preempt_count_up` and `preempt_count_down`; and `syscall_cost` read on both
benches before and after.
