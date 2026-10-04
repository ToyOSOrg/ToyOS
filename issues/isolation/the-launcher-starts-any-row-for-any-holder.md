---
status: open
kind: defect
opened: 2026-10-03
---

# The launcher starts any declared row for any holder of it

A holder of a `launcher` connector can start **any** `[programs]` row and is
handed that row's whole authority. Three facts compose:

- **`launcher` is inherited, not granted per child.** A direct spawn copies the
  caller's `svc` namespace into the child (`inherited_namespace`,
  `rust/library/std/src/sys/process/toyos.rs`), and std routes every declared
  program through the launcher when the caller holds one and endowed nothing
  itself (the routing rule in `Command::spawn`/`Command::launch`, same file). So
  every program in a shell-started subtree holds the shell's `launcher`.
- **The supervisor never checks which rows a caller may start.** `serve_launch`
  (`userland/supervisor/src/main.rs`) resolves `request.program` to a declared
  row and starts it holding that row's `receives`, `devices`, `syscap` and
  `slots` — for any caller that reached the port. A plain app launched from the
  shell can therefore launch `/system/bin/swap` and receive the swap connector,
  or `/system/bin/update` and receive the idle slot's partition claims.
- **`accept_swap` checks the requester's own digest, not its identity.** It
  verifies the service exists, is a running `[boot] start` service, and that the
  staged bytes match the requester's `digest` — never which program asked
  (`accept_swap`, same file). The swap connector is gated to `/system/bin/swap`
  by the build (`held_by_their_holders_alone`, `src/build.rs`), but fact one and
  fact two reach that row anyway. So any program can rewrite any boot service's
  binary.

The per-program-views track names the same shape in the service namespace ("the
swap port leaking to whatever sshd spawns",
`issues/isolation/every-program-sees-only-the-files-it-was-given.md`), and it is
distinct from where a swapped binary lives
(`issues/isolation/a-swapped-binary-lives-where-any-process-can-rewrite-it.md`):
that one is which bytes a declared swap runs, this one is who may invoke a
declared row at all.

**The owner's ruling:** each program may start only the rows its own row lists;
swap and update of system programs only from the login session.

**Owner.** The orchestrator.

**Exit condition.** A guest test that is red until both halves of the ruling
hold: a launch of a row the caller's own row does not list is refused —
`/system/bin/swap` and `/system/bin/update` through an inherited `launcher`
among them — and a swap and an update of a system program asked from outside
the login session are refused.
