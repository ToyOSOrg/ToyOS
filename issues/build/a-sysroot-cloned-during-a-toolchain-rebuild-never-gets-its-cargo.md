---
status: open
kind: tooling
opened: 2026-09-27
---

# A sysroot cloned during a toolchain rebuild never gets its cargo

The primary checkout's `rust/build/aarch64-apple-darwin/stage2/bin` was
recreated at 22:00 on 2026-09-27 and, read while a `toyos-build --build-only`
ran in the primary, held `rustc` and `rustdoc` and no `cargo`; the `cargo` link
appeared at 22:08. At 22:03:59 a `cargo test --test toyos-build` in the
`wt/toyos-nokthread` worktree rebuilt std and published
`rust/build/sysroots/5dc157f7fac727be` cloned from that `bin/`, so it has
`rustc` and `rustdoc` and no `cargo`. The key is found again on every later
run and nothing re-provisions it: every harness run from that worktree since
panics at `tests/toyos.rs:2970` with `the toyos toolchain at
.../sysroots/5dc157f7fac727be/bin is missing cargo` on the C corpus, before any
test runs.

**Evidence:** the two directory listings and the harness log above, read on
the dev host; not reproduced on purpose.

**Exit condition:** a sysroot is never published without the provisioned
`cargo`, and a run that finds a published one without it provisions it or
refuses by name at the toolchain step rather than inside the corpus.
