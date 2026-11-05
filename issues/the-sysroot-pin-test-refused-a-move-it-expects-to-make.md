---
status: open
kind: tooling
opened: 2026-11-05
---

# The sysroot pin test refused a move it expects to make, once

`sysroot::tests::the_primary_s_fork_checkout_is_moved_to_its_pin`
(`build/sysroot.rs`) redded once in a `cargo run -- --ci host` run at
ce5d8469d, on a host other builds were loading, and passed run alone at the
same revision.

What the log shows: after three moves of the test's own fork checkout
(0a69 to 3a47, back, and to 3a47 again), the fourth, the test's `pin(&c1)`
before its deliberate edit, refused with "is at 3a47… with uncommitted work,
and this tree pins the fork at 0a69…", naming one entry,
`1 .M N... 100644 100644 100644 82a3da18… 82a3da18… library/std/src/lib.rs`.
The test had written nothing to that file before the refusal. Why `git
status` reported it modified is not established.

Evidence: #1193's host CI log at ce5d8469d and the test alone passing there:
https://github.com/ToyOSOrg/evidence/tree/38c6ab9b31d7e7eff1b3ffc9be1180c3130f29c7/pr-1193/r3/logs

Owner: the build track. Exit: the cause found and fixed, or the test shown to
hold under the load it redded in.
