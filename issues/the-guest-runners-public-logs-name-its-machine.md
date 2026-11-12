---
status: owner
kind: question
opened: 2026-11-12
---

# The guest runner's public logs name its machine

`guest-rows.yml` runs on our own machine, and its logs, step summaries and
`serial` artifacts are public. The rows job masks its CPU's brand string,
signature and microcode revision, and its host name, in all it says and the
serial logs it keeps (`build/ci.rs`'s `Identity`). Three things it cannot mask
are the owner's.

1. **The runner names the host.** Before the job's first step the runner
   writes `Machine name:` into every job's log, and the host's name carries a
   personal name (run 43143902443). The runner's user has no sudo, so only the
   owner can give the host, or the runner's view of it, a name that identifies
   nobody, as the workflow's header requires.
2. **Earlier runs carry the values.** The 446 runs from 2026-11-04 to
   2026-11-12 carry the host name and, in each instrument line and summary,
   the CPU's brand string; a red row's echoed serial carries its signature and
   microcode revision, as do the 72 `serial` artifacts unexpired on
   2026-11-12. The repository keeps logs 90 days and these artifacts 7.
   Deleting them sooner is the owner's decision, and nothing has been deleted.
3. **What a guest measures of its host is not masked.** A KVM guest's TSC
   rate, XSAVE layout and feature words are its host's, and narrow the CPU
   down without naming it.

**Exit**: the host's name identifies nobody, the owner has decided the earlier
runs' fate, and has ruled whether the measured values identify the machine.
