---
status: open
kind: defect
opened: 2026-09-28
---

# test-runner holds `logread` where no log builtin runs

`logread` in a `[programs.test-runner]` row is authority for the builtins that
read the log inside test-runner (`log-gate`, `log-storm`, `log-close`,
`userland/test-runner/src/main.rs`'s `BUILTINS`). Every boot that runs one is a
`tests/testcases` boot (`tests/common/logread.rs` boots the machine tests' config;
`tests/toyos.rs`'s one job list naming `log-close` is `tests/testcases`). Seven
other manifests grant it anyway: `tests/partclaimcase`, `tests/blockdcase`,
`tests/doommusiccase`, `tests/logrotatecase`, `tests/metalcase`, `tests/netcase`
and `tests/sshdcase`. Their comments gave the
log gate as the reason, which none of those boots runs; the comments are gone
and the grants are not.

`partclaimcase` and `blockdcase` also grant `dup`, so there every child
test-runner spawns receives a `SysCap` duplicate carrying `LOG` as well.

**Evidence:** `rg -n '"log-gate"|"log-storm"|"log-close"' tests userland`, and
`rg -n logread -g system.toml tests`.

**Exit:** every `logread` in a test manifest is one a program on that boot
uses, or the row says which use it is for.
