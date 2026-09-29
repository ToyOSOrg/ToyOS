---
status: open
kind: defect
opened: 2026-09-29
---

# The x86-64 toybox ships two applets that only panic

`userland/toybox/src/main.rs` puts `fp_isolation` and `first_entry` in one
`commands!` list for every architecture, so the x86-64 toybox that ships in
every image answers both names, and `userland/toybox/src/arch/x86_64.rs`'s
body for each is a `panic!` naming where x86-64's probe lives or is owed
(`test_rs_fpu_isolation`,
`issues/isolation/a-new-x86-thread-enters-ring-3-holding-kernel-register-values.md`).
A shipping binary carries two commands whose only behaviour is to die.

**Exit condition**: the x86-64 toybox answers neither name with a panic —
each is left out of its `commands!`, or runs a probe of x86-64's own.
