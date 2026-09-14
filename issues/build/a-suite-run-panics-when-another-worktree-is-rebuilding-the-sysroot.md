---
status: open
kind: tooling
opened: 2026-09-13
---

# A suite run panics on the toolchain another worktree is rebuilding under it

`src/toolchain.rs:940` asserts that the `toyos` toolchain's `stage2/bin` holds a
`cargo`, and panics the whole run when it does not:

```
thread 'main' panicked at src/toolchain.rs:940:5:
the toyos toolchain at /Users/jan/Dev/jan/toyos/rust/build/aarch64-apple-darwin/stage2/bin
is missing cargo, so rustup answers for it by falling back to another toolchain
and narrating it on every invocation.
provision_toolchain_cargo is the step that puts them there, and it did not.
```

**It is another worktree's rebuild, not a broken checkout.** Both times the
build lock's own log names the holder one line above:

```
[build-lock] still waiting for the build lock (shared, test binaries), 60s so
  far — held by pid 93637 (rebuild the shared sysroot from a linked worktree)
[build-lock] acquired (shared, test binaries) after 81.4s

thread 'main' panicked at src/toolchain.rs:940:5:
```

The rebuild replaces `stage2/bin` while the waiter is queued; the waiter takes a
*shared* lock the moment the exclusive one is released and reads a directory the
rebuilder has emptied and not yet refilled, or refilled without the cargo hard
links. Taking the lock is therefore not enough: what the assertion reads is
outside it.

Twice in one session, on `cargo test --test toyos-build -- --metal
--metal-readback=...`, EXIT=101 each time, with a plain re-run green each time —
so it costs a whole staging run (tens of minutes of image builds) and nothing
else. Any long run in a worktree can take it.

## What would close it

`provision_toolchain_cargo`'s invariant holding across the handoff — the links
placed before the exclusive lock is dropped, or the assertion reading under the
same lock that the rebuild holds. A re-run is not a fix: the assertion is right
about what it saw, and the window is what has to go.
