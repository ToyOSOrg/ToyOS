---
status: open
kind: defect
opened: 2026-09-26
---

# `CARRIES` misses `netd_udp_any_address` and `netd_lookup_let_go`

`tests/toyos.rs`'s `CARRIES` table names, for each machine test not built by
the generic `test_rs_<name>` discovery, which catalogue keys its boot must
carry (`carried_by` unions the rows of every test in a scheduling group and
hands the result down as that group's whole `rust_bins`). `netd_udp_any_address`
and `netd_lookup_let_go` have no row. When either is the only member of its
group — filtered alone, or re-run alone after a parallel failure — its boot
carries zero rust binaries and `netcase_against_host`'s `bins.is_empty()`
check reds it `"<name> was not built"`, even though the binary was compiled
and sits in `tests/toyos-rust-tests/target/x86_64-unknown-toyos/toyos/`.

Reproduces on `wt/toyos-dns` at `cb1c488b`, before PR #511's merge of
`origin/main` touched anything: `cargo test --test toyos-build --
netd_udp_any_address` and `... -- netd_lookup_let_go` both red this way in
isolation; `cargo test --test toyos-build -- netd_udp_refused` (which does
have a `CARRIES` row) passes.

Exit condition: `CARRIES` gets a `("netd_udp_any_address", &["test_rs_netd_udp_any_address"])`
and a `("netd_lookup_let_go", &["test_rs_netd_lookup_let_go"])` row (matching
`netd_udp_refused`'s), and both tests pass filtered alone.
