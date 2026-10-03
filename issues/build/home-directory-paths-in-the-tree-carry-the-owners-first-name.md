---
status: owner
kind: question
opened: 2026-10-03
---

# Home-directory paths in the tree carry the owner's first name

Root `CLAUDE.md` keeps what identifies the owner's machines or network out of
the tree, and `src/sourcegate.rs` reds on the shapes it names: a MAC address,
a public address, a serial, a hostname with his name. A home-directory path is
none of them, and seven tracked files carry `/Users/<first name>/…`, each
quoted from a build or a run on the development machine
(`git grep -c -a -E '/Users/[a-z]+'`):

- `src/toolchain.rs`, seven lines of dep-info fixtures, and `src/metal.rs`,
  one line of cargo's output: test input, where any path would do.
- `issues/build/the-shipping-kernel-actuator-gate-reads-the-checkouts-own-path.md`,
  `issues/hardware/a-boot-with-no-kernel-log-is-not-no-boot-complete.md`,
  `issues/kernel/a-job-list-hangs-with-interrupts-on-and-the-deadline-ends-it.md`
  and
  `issues/kernel/mutual-kill-panicked-on-the-t14-with-stdio-slot-1-not-a-log-ring.md`:
  five lines naming a worktree or a log by its path.
- `toyos-symbols/tests/fixtures/input-test.bin`, 51 strings
  (`strings -a … | grep -c /Users/`): the paths the compiler recorded when the
  fixture was built, which only a rebuild with the prefix remapped removes.

`userland/sshd/src/main.rs` also writes a key comment
`<first name>@some-other-laptop` in a test.

The name itself is public: `LICENSE-MIT` carries it in full.

## The question

Is a path or an account name that carries the owner's first name private, as
a hostname carrying it is?

## Exit condition

The owner's answer. If it is yes: the seven files carry a neutral path, the
fixture is rebuilt with its prefix remapped, and `src/sourcegate.rs` reds on
the shape; if it is no, this file is deleted.
