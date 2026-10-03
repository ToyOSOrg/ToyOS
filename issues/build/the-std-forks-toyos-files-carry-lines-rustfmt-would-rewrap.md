---
status: open
kind: defect
opened: 2026-10-03
---

# The std fork's ToyOS files carry lines rustfmt would rewrap

Under `rust/rustfmt.toml`, rustfmt rewrites seven places in four of the `rust`
fork's ToyOS files under `library/std` at `a0d44493347`, each file checked on
its own text (`rustfmt --check --config-path rustfmt.toml --edition 2024`,
stable 1.9.0, which skips the config's unstable import options):

| file | places |
|---|---|
| `src/os/toyos/fs.rs` | 1 |
| `src/sys/fs/toyos.rs` | 2 |
| `src/sys/pal/toyos/mod.rs` | 2 |
| `src/sys/process/toyos.rs` | 2: the three `setup_slot` calls and the `Outcome::Gone` arm |

A fork change is written as upstream would take it, and upstream formats
`library/std` with that configuration.

**Owner**: the `rust` fork.

**Exit**: rustfmt under `rust/rustfmt.toml` changes no ToyOS file under
`library/std`.
