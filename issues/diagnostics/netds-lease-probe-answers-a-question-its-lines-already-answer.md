---
status: open
kind: tooling
opened: 2026-09-16
---

# netd's lease probe answers a question its own lines already answer

netd's `--exit-with-lease` brings the card up and serves exactly as the
shipping boot does for a bounded window, leaves `/log/lease.txt` on the log
volume one durable line at a time (the bring-up's own words, every link change,
the lease, what the driver and the MAC counted each way), and ends with
`toyos_i219::lease::Verdict`'s code. It existed because every line netd printed
about the same things was a console write, and on the T14 a console write
reached no file. Every program's lines reach `/log` through its log ring now,
and the talking boot's `lan_dhcp_lease` judge reads the lease off netd's own
lines. What still arms the probe is `tests/lanleasecase`, the `lan_lease_report`
metal row: a T14 flash of its own, because netd ends inside it.

## Owner

The I219 bring-up's author, and after it the network track.

## What would close it

The probe deleted: `tests/lanleasecase/system.toml`, its row and its gate in
`src/build.rs`, the `lan_lease_report` metal row in `tests/toyos.rs` with
`LANLEASECASE`, `lan::leased_on_metal`, `Readback::log_volume_file` and
`tests/common/volumes.rs`, netd's `--exit-with-lease` and
`userland/netd/src/report.rs`, `toyos-i219/src/lease.rs`'s report lines, and
`toyos_i219::phy::Outcome`, whose tests then assert the refusal itself.

An earlier form of this arm, `--exit-with-phy-outcome` on `d409139f^:tests/lanphycase`,
ended right after the bring-up with the PHY's outcome; its codes are still
`Verdict::NotLeased`'s, so a code read off one of its boots means what
`toyos_i219::phy::Outcome` says.
