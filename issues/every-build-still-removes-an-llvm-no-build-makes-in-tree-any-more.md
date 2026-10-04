---
status: open
kind: tooling
opened: 2026-09-29
---

# Every build still removes an in-tree LLVM that no build makes any more

`llvm::retire_in_tree` removes a bootstrap build directory's own `<host>/llvm`,
`<host>/lld`, `<host>/ci-llvm` and `cache/llvm-*`. It runs at four sites on
every build: the primary's `reassemble`, `compiler::place`, `compiler::choose`
on the way back to the primary's compiler, and the std build
(`sysroot::prepare_std_build`).

Since the LLVM store, no build makes any of these. Every compiler build links
the store's `llvm-config`, and the std build sets `download-ci-llvm = false`.
What the four sites remove is what a build directory kept from before the
store. After a checkout's first build at that code, they remove nothing, but
they keep asking, on every build, a `read_dir` of `cache/` and a `stat` of
each name.

Exit: delete `retire_in_tree`, `in_tree` and the four call sites once no
checkout that builds holds a build directory made before the store. A host
cannot know that for any other host, so the owner sets the date.
