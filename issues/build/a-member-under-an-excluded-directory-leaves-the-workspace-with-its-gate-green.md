---
status: assigned
kind: tooling
opened: 2026-10-03
---

# A member under an excluded directory leaves the host workspace with its gate green

`src/hostws.rs`'s `crate_dirs` prunes each `exclude` entry's directory whole,
so `every_crate_in_the_tree_joined_the_workspace_or_was_excluded_by_name`
never sees a `Cargo.toml` under one. Cargo does not: a directory `members`
names is a member even under an excluded one. That line is then the only thing
that makes it one, and dropping it takes the package's tests out of
`--ci host` with the gate green. `crate_dirs`'s doc, "pruned exactly as cargo
prunes them", is false.

**Evidence:** at `0d3ec4388`, `exclude` named `tests` whole and `members`
named `tests/libc-arch`. Its `--ci host` ran `toyos_libc_copies`'s 35 tests.
With that member line dropped (`mutation-member.patch`, posted on #692),
`cargo test -p toyos-build --lib hostws::` exited 0. #692 ends that instance
by excluding the two packages under `tests/` that keep their own resolution by
name; at its head `31d730b63`, no member sits under an `exclude` entry.

**The instance:** step 2 of
`issues/build/code-used-by-one-program-lives-in-that-program.md` makes
`kernel/loom` and `kernel/sim` members, and `exclude` names `kernel` whole.

**Exit:** dropping from `members` a line that names a directory under an
`exclude` entry reds a `--ci host` gate, shown by that mutation on
`kernel/loom` or `kernel/sim` once step 2 has moved them, and `crate_dirs`'s
doc is true of the walk. Owner: the orchestrator, who owns the track's step 2.
