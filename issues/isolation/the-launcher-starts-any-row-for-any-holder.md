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

This is the service-namespace shape of the hole the per-program-views track
names ("the swap port leaking to whatever sshd spawns", PR #484's review,
`issues/isolation/every-program-sees-only-the-files-it-was-given.md`), and it is
distinct from where a swapped binary lives
(`issues/isolation/a-swapped-binary-lives-where-any-process-can-rewrite-it.md`):
that one is which bytes a declared swap runs, this one is who may invoke a
declared row at all.

**Owner.** The per-program-views track
(`issues/isolation/every-program-sees-only-the-files-it-was-given.md`), which
badges a program's authority per row.

**Exit condition.** `launcher` is badged per row: a launcher connector names
which rows its holder may start, `serve_launch` refuses a row the badge does not
name, and a swap gates on the requester's badge rather than on its digest alone.
A test fails until then: a program whose row grants no launcher badge for
`/system/bin/swap` launches it through an inherited `launcher` and the supervisor
refuses the launch.
