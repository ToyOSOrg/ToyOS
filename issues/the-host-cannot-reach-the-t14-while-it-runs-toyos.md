---
status: open
kind: tooling
opened: 2026-10-08
---

# The host cannot reach the T14 while it runs ToyOS, and no T14 row reads its wired card

The development host reaches the T14 only while it runs Ubuntu. Under ToyOS no
connection the host opens to it arrives, so every row whose boot the host had
to talk to was red, and they are deleted as a red test is.

## Measured

The T14, boot `lantalkcase`, at `main`'s `6f87cdb9c` and at `c84f5fafe` on
2026-10-08. Both `verdict.txt`:

```
refused
the boot did not say over its own cable what a talking boot owes:
  the stream never opened
```

`toyos-metal` refused the whole boot, so its three rows were red with that
text: `lan_talk` and `lan_dhcp_lease`, which read what the host heard, and
`lan_message_delivery`, which read only the stick.

## Deleted

`809c33c0c` holds all of it, and is the commit that restores it:

- the rows `lan_talk`, `lan_dhcp_lease` and `lan_message_delivery`, their
  judges (`tests/common/lan.rs`, `tests/common/logstream.rs`) and
  `lan_dhcp_lease`'s fixture (`tests/checks/lan.rs`);
- the boot `lantalkcase`: `tests/lantalkcase`, its job `lan_talk_hold`, and its
  rows in `tests/metal/lenovo-20w0003amz.toml`;
- the harness's talking boots: `Arm::talk`, `Arm::nic`, the key minted beside
  an image (`tests/common/ssh.rs`), `Readback::talk` and `Readback::wire_mac`;
- `toyos-metal`'s half: `--talk` and `--nic`, `Refusal::Talk`, `Refusal::Wire`
  and `Refusal::Cable`, the host's reader of the log a machine serves and its
  conversation (`src/metaltalk.rs`), its ping (`src/icmp.rs`), and the lease,
  link and message readers (`src/lan.rs`);
- the swap of a running service from the host, by the owner's ruling "Delete
  the whole chain": `toyos-metal --swap` and `--binary`, `Refusal::Swap`,
  `src/metalswap.rs`, the harness's ssh client `tests/ssh-client-host` with
  `build::build_host_judges`, `build::ssh_client_host` and
  `build::copy_guest_program`, and `toyos-swap`'s readers of the supervisor's
  words (`heard`, `outcome`, `Word::is_final`). `/system/bin/swap`, the
  supervisor's half and sshserver ship as before, and nothing on a host asks
  them for a swap.

`lan_message_delivery` could not move to a boot that does not talk without a
new job: the card's first message arrived 8.499 s into the last boot read,
with its link, so the boot has to be held open until then, and the one hold
the tree has is the flat sleep
`issues/lan-hold-holds-a-boot-open-for-a-flat-twenty-seconds.md` records.

## What stands in the meantime

No T14 row reads netstack on the I219, and none reads a message the card
raised. `claim_reuses_its_remapping_entry` claims the function and gives it
back twice and takes no traffic. So the calls #763 moved onto a claim's
binding — the I219's interrupt records, its polls and its DMA grants, driven
at rate — have not been read on real hardware since they landed; under QEMU
`iommu_virtio_platform` and the suite's network tests drive the same `pcidev`
functions through virtio-net's claim.

What reads the card next is not these rows restored. The owner ruled that the
rows judged from the stick alone — the T14 reaching its router and the
internet — are built on ToyOS's own network stack ("no smoltcp."), so they
arrive with `issues/toyos-has-its-own-network-stack.md`.

## What the restored chain still owes

Each was an issue of its own and is true of `809c33c0c`'s code, so it comes
back with it:

- A swap on the T14 lives inside a metal boot's bound: every metal image
  carries `boot-deadline=` (`toyos_tco::WEDGE_BOUND_MS`) and a runner whose
  list ends in `reboot` (`toyos_tco::JOB_BOUND_MS`), so the loop the owner
  asked for — netstack rebuilt on the host and swapped in, over and over —
  has the span from the first lease to the runner's bound, and then a flash.
  Owed: a boot staged for the swap loop whose hold is not the runner's, and a
  ruling on what bounds it.
- `Stream::redial` asks again with no event to wait on: a swap of netstack
  ends the host's stream with no FIN and no reset, nothing the machine sends
  says when `logkeeper` admits a reader again, and every dial turned away is
  asked again at once (through QEMU's forward, 8125 dials in 7019 ms). Exit:
  the host waits on something the machine sends, and dials once per event.
- `open` sets `on_the_link` on the first failed dial and never clears it, so
  every refusal is followed by a multicast question, faster than RFC 6762
  §5.2's floor. Exit: only a dial that finds nobody home asks the link again.
- `serve` records no `unopened` for a first dial closed before a line, so
  `Stream::wait_for_connection` waits out its whole bound. Exit: that close
  ends the wait at once, shown by a host test that fails on the base.
- `Stream::wait_until` is woken only by a line, so `metalswap::swap` waits out
  its window on a stream that can carry no more (124 s at PR #566's
  `7e06a657`). Exit: it returns once the stream's `dialing` goes false.
- The ssh client is no second implementation: its lock pins the `russh` commit
  `Cargo.lock` pins for sshserver, so a defect in the fork's protocol code is
  on both ends of every exchange.

Owner: the orchestrator, which holds the T14.

**Exit**: a connection the development host opens to the T14 while it runs
ToyOS is answered, read as `lan_talk`, restored from `809c33c0c`, green from
the T14, and each line of "What the restored chain still owes" is fixed or
filed again as its own issue in that restore.
