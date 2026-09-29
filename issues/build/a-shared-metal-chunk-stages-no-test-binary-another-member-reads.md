---
status: open
kind: tooling
opened: 2026-09-29
---

# A shared metal chunk stages no test binary another member reads, so `dlopen_dedup` finds no `test_rs_std_tls`

`dlopen_dedup`'s last arm reads `/system/bin/test_rs_std_tls` as a
`DT_NEEDED`-carrying binary. `tests/common/metal.rs`'s `build` puts on an
image the binaries its own jobs name, every `.so`, and only those `RUST_SKIP`
helpers some job's source names; `std_tls` is a test and not a helper, so it
is on an image only when it is a job there. `sized` cut the shared list into
`shared` and `shared-2`, and `std_tls` ran on `shared-2` while
`dlopen_dedup` ran on `shared`.

## Measured

The full T14 run of `main` at `7e151819`
(`/Users/jan/.claude/jobs/2280e09e/tmp/scratchpad/orch/main-metal-full.log`,
EXIT=1), boot `shared`, after the two dedup checks passed:

```
PASS: 8 concurrent loads of one name returned one handle (1)
thread 'main' (1) panicked at src/bin/dlopen_dedup.rs:42:63:
read /system/bin/test_rs_std_tls: entity not found
[... cpu5] exit: test_rs_dlopen_dedup pid=70 code=101 cpu=197ms
```

`===TEST_START test_rs_std_tls===` is at line 20467 of that log, inside the
`shared-2` boot.

## Exit condition

A binary a staged job's source names by path is on that job's image whether
or not it is itself a job there, and a T14 run exits `dlopen_dedup` 0; then this file is deleted.
