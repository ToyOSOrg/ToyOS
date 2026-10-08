---
status: open
kind: tooling
opened: 2026-10-08
---

# Nothing builds the ToyOS-hosted rustc

No build makes a rustc that runs on ToyOS, and no config can ask for one. The
primary checkout's in-place build of it, the `hosted-rustc` key of
`system.toml` and the image's collection of its files went with the primary's
in-place compiler: the owner, of the in-place hosted rustc build and the rustup
link, "Drop both for now (Recommended)". No tracked config set the key.

What that build left owed, which the one that replaces it owes too:

- It was the primary's alone, built from the primary's `rust/` under the
  primary's configuration, so no branch could put the hosted rustc it changes
  into a guest before it landed, and a worktree whose configuration alone
  differed shipped the primary's without a word.
- Nothing keyed it on std's sources: an edit to `sdk/std` or to the fork's
  `library/` left a built one standing, and an image carried its `libstd-*.so`
  beside the rlibs of the build's own sysroot.
- It was unmeasured since std's ToyOS backend left the fork for `sdk/std`: no
  hosted rustc was built whose std reaches the backend through the fork's
  `#[path]` arms.
- Nothing read its licences: a config that set the key was refused for that.

Owner: `issues/toyos-builds-itself.md`, M3.

**Exit**: a hosted rustc is a product of the host's store (`src/keystore.rs`),
keyed as `src/compiler.rs` keys a compiler and on the sources of the std it
runs on, and built by whichever checkout first names it; an image whose config
asks for it carries that one, with its licences read; and the `libstd-*.so` it
carries holds `sdk/std/sys/` paths and no `sys/pal/toyos`.
