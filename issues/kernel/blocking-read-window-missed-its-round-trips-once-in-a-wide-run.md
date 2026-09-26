---
status: open
kind: defect
opened: 2026-09-26
---

# `blocking_read_window` missed its round trips once in a wide run

`cargo test` (the fast tier) on the dev host at `8d7ef76a` on the TCP
robustness branch (#520), which touches nothing under `kernel/`, `toyos/`,
`toyos-abi/` or the runner: `blocking_read_window` red with "only 25 of 500
round trips completed inside 3s — a wake was not delivered". Green when the
harness re-ran it alone in the same run. `cargo run -- --known-red
blocking_read_window` answers that it is not quarantined.

The test is the lost-wake canary (`test_rs_blocking_read_stress`) on a boot
armed with `watch-window`, which holds the watch's window open; 25 of 500 is
a reader that stopped being woken, not one that was slow.

Exit condition: a capture of a red run that names the wake that was not
delivered, or the defect fixed where that capture points.
