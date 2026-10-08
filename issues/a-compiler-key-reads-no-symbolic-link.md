---
status: open
kind: tooling
opened: 2026-10-01
---

# A compiler key reads no symbolic link

`compiler::key` reads the paths of `compiler::KEYED` through
`sysroot::tree_identity` with `Links::Skipped`, so a symbolic link there is
in no key: retargeting one, or editing what it names outside those paths,
keeps the old compiler. Refusing a link there, as the freestanding key does,
refuses every compiler build: `git -C rust ls-files -s` over `KEYED`'s eleven
paths at fork commit `6d6ad8c7190` lists 5 entries of mode `120000`, all
under `src/tools` (`rustc_tools_util`'s and `lsp-server`'s `LICENSE-APACHE`
and `LICENSE-MIT`, and rust-analyzer's `AGENTS.md`). Hashing a link's target
text instead moves the key of every compiler once.

Owner: the first step of
`issues/the-forks-pin-is-a-file-and-a-worktree-checks-no-fork-out.md` ("The
fork's objects are the store's"), which reads a key's fork parts as git tree
ids, where a link's target text is in the key by construction, and moves
every compiler key anyway; the orchestrator briefs it.

**Exit**: `Links::Skipped` is deleted, and a test in which retargeting a link
under a keyed path moves the compiler's key.
