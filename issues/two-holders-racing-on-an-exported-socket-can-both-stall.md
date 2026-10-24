---
status: open
kind: defect
opened: 2026-10-24
---

# Two holders racing on an exported socket can both stall

`netstack_holders_exported_to_children`'s last step (`tests/guest/rust-tests/src/bin/socket_export.rs`,
`race`) has the parent and a second child each block on a listener and a
datagram socket they both hold, while the harness connects twice and sends two
datagrams. Now and then neither process goes on: the racer is spawned
(`spawn: /system/bin/test_rs_socket_export`), the first part's line is printed,
and the guest says nothing more until the harness's ceiling (952–970 s guards
expired).

QEMU x86-64 under TCG on an aarch64 Mac, the test alone, `--nocapture`: one
stall in six runs at `32046264b` (main), two in six at PR #1060's head, whose
change touches no socket or netstack code. Each stalled run's last guest lines
are the racer's spawn and netstack's mDNS line. Logs: PR #1060's evidence,
`qemu/netstack-*`.

PR #1062 saw the same stall, the same last lines, at load averages of 20 to
28: two of three runs at its branch (`ebac17a93`), whose change touches no
socket or netstack code, and one of one at `2a5b446b0` (main). Logs: PR
#1062's evidence, `guest/socket-export-*`.

What blocks is not established: no guest line says whether either racer sent
its `racing` datagram, which the harness waits for before it connects.

**Exit**: the race finishes in every run of a repeated QEMU loop, with the
blocked step named by a line the guest prints, or the cause found and fixed.
