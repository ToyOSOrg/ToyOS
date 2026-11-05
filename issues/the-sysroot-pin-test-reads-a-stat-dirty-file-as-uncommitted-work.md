---
status: open
kind: tooling
opened: 2026-11-05
---

# The sysroot pin test reads a stat-dirty file as uncommitted work

`sysroot::tests::the_primary_s_fork_checkout_is_moved_to_its_pin`
(`build/sysroot.rs`) redded in a `cargo run -- --ci host` run at ce5d8469d on
a host under other builds' load, and passed alone at the same revision. Its
refusal named `library/std/src/lib.rs` as uncommitted work in the test's own
fake fork, with `git status --porcelain=v2` reporting it `.M` over the same
blob on both sides (`82a3da18…`): the file was dirty by its stat alone, git's
racy-timestamp case, and not by content. The check in the "move the fork
checkout to its pin" step reads that status as work and panics, so the test
reds whenever the test's own checkout lands in the same timestamp tick as the
index write.

Evidence: #1193's host CI log at ce5d8469d, and the test alone passing there:
https://github.com/ToyOSOrg/evidence/tree/38c6ab9b31d7e7eff1b3ffc9be1180c3130f29c7/pr-1193/r3/logs

Owner: the build track. Exit: the pin check tells content from stat (for
instance by refreshing the index first), and the test no longer reds on a
stat-only change.
