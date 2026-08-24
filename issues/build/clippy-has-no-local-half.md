---
status: open
kind: defect
opened: 2026-08-19
---

# Clippy reds in CI and nowhere before it

Default clippy with warnings denied now runs on every pull request — the
`clippy` step of `host-tests.yml`'s `host` job, over the host workspace, the
kernel and the bootloader. Nothing runs it on the machine the code was written
on, so the first thing that tells an author about a finding is a red check
several minutes after a push.

**No local half was added, deliberately.** The obvious homes are both wrong:

- `cargo test --lib` is the build system's own gates and finishes in ~2.6 s.
  Three clippy invocations are 25 s cold on the dev host and would have to shell
  out to cargo from inside a test, which is a build inside a test run.
- `src/build.rs` would put it in the `cargo run` dev loop, which agents do not
  use and which is the path the owner watches a window on.

What would actually fit is a pre-push hook, and this repository has no hook
mechanism at all — adding one is new machinery with its own failure modes
(hooks are not in VCS, so a hook is a rule that silently does not exist on a
fresh checkout). That is a decision worth making once, for hooks in general,
rather than smuggling it in under clippy.

Until then the three commands are in the `clippy` step's own comment, and
running them by hand before a push is the whole of the local half:

```
cargo clippy --workspace --all-targets --keep-going -- -D warnings
(cd kernel     && cargo clippy --target x86_64-unknown-none -- -D warnings)
(cd bootloader && cargo clippy --target x86_64-unknown-uefi -- -D warnings)
```

**2026-08-25: promoted, and the commands above are now stale.** The `clippy`
step in `.github/workflows/host-tests.yml` grew an `$ADOPTED` per-area lint
list, a fourth invocation (kernel with `boot-actuators,test-actuators`), a
fifth (`toyos-abi` unsafe blocks), and an extra
`-W clippy::undocumented_unsafe_blocks` on the bootloader's — none of which
this file's copy-paste block carries. Whoever next runs the local half by hand
should copy the five commands from the workflow step directly rather than from
here, and the no-local-half decision itself is unchanged and still open.
