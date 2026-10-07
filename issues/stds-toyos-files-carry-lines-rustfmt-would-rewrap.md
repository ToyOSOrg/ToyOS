---
status: open
kind: defect
opened: 2026-10-03
---

# std's ToyOS files carry lines rustfmt would rewrap

Under `rust/rustfmt.toml`, the rustfmt `rust/src/stage0` pins (`1.9.0-nightly
77cf889bc1`, which applies the config's `group_imports` and
`imports_granularity`) rewrites seven places in four of std's ToyOS files,
measured at `rust` fork commit `a0d44493347`, which held them under
`library/std`, each file checked on its own text (`rustfmt --check
--config-path rustfmt.toml --edition 2024`). Stable 1.9.0, which skips those
two options, rewrites the same seven:

| file | places |
|---|---|
| `sdk/std/os/fs.rs` | 1 |
| `sdk/std/sys/fs.rs` | 2 |
| `sdk/std/sys/pal/mod.rs` | 2 |
| `sdk/std/sys/process.rs` | 2: the three `setup_slot` calls and the `Outcome::Gone` arm |

std's ToyOS backend is written as upstream would take it, and upstream formats
`library/std` with that configuration.

**Exit**: the rustfmt `rust/src/stage0` pins, under `rust/rustfmt.toml` with
its two import options, changes no file under `sdk/std`.
