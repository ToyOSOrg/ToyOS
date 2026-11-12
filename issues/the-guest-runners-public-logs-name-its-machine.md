---
status: assigned
kind: tooling
opened: 2026-11-12
---

# The guest runner's public logs name its machine

`guest-rows.yml` runs on our own machine, and its logs, step summaries and
`serial` artifacts are public. The rows job masks its CPU's brand string,
signature and microcode revision, and its host name, in all it says and the
serial logs it keeps (`build/ci.rs`'s `Identity`). Two things remain, and
guest runs continue meanwhile.

1. **The runner names the host.** Before the job's first step the runner
   writes `Machine name:` into every job's log, and the host's name carries a
   personal name (run 43143902443). No job code masks it: the owner renames
   the host to a name that identifies nobody, as the workflow's header
   requires.
2. **Earlier runs carry the values.** Each guest-rows run since 2026-11-04
   carries the host name, and each that reached its instrument step the CPU's
   brand string there and in its summary. A red row's echoed serial carries
   the signature and microcode revision, as do the serial logs of an x86-64
   KVM boot that a `serial` artifact keeps (run 43143902443's does). Once the
   host is renamed, the orchestrator deletes the logs and `serial` artifacts
   of every run before the rename, and keeps the runs themselves.

A KVM guest's TSC rate, XSAVE layout and feature words are its host's. The
owner ruled on 2026-11-12 that they do not identify the machine, so they are
not masked.

**Exit**: the host's name identifies nobody, and no run from before the
rename keeps a log or a `serial` artifact. Owner: the owner renames the host;
the orchestrator deletes the logs and artifacts after the rename.
