---
status: open
kind: tooling
opened: 2026-09-29
---

# Every build still removes an in-tree LLVM that no build makes any more

`llvm::retire_in_tree` removes a bootstrap build directory's own `<host>/llvm`,
`<host>/lld`, `<host>/ci-llvm` and `cache/llvm-*`. It runs at two sites:
`compiler::place`, and the std build (`sysroot::prepare_std_build`) on every
sysroot build.

Since the LLVM store, no build makes any of these. Every compiler build links
the store's `llvm-config`, and the std build sets `download-ci-llvm = false`.
What the two sites remove is what a build directory kept from before the
store. After a checkout's first build at that code, they remove nothing, but
they keep asking, a `read_dir` of `cache/` and a `stat` of each name.

Exit: delete `retire_in_tree`, `in_tree` and the two call sites once no
checkout that builds holds a build directory made before the store. A host
cannot know that for any other host, so the owner sets the date.
