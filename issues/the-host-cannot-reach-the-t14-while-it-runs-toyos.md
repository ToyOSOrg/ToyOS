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
  words (`heard`, `outcome`, `Word::is_final`);
- the machine's end of the stream, which no image could reach once
  `lantalkcase` went: `logkeeper`'s network half (`serve_network`, `bind_port`,
  `Carrier`, `Hub::carrier`, the network readers' count, the `stream` word of
  its `inspect` snapshot), `toyos_logstream::PORT`, `CARRIER`,
  `CARRIER_LEAVING` and the `SWAP` frames the supervisor sent `logkeeper` to
  close that listener across a swap of netstack, and the build's
  `no_shipped_image_serves_the_log_on_the_network`, which then guarded nothing;
- sshserver's second key file, `/system/etc/ssh_authorized_keys`, which only a
  talking boot's staging wrote. It reads `authorized_keys` under its own state
  directory and nothing else.

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

`/system/bin/swap` and the supervisor's swap path stay, because a shipped
machine can ask for one: `system.toml` makes the desktop a login session whose
shell starts `swap`, and gives a login over sshserver the same. No test
performs a swap: `launch_authority` reads the rows that refuse to start it and
nothing reads one accepted, stopped, started or restored.

Two things about that swap and that login are unread at this head:

- Whether sshserver's port is turned away after a swap of netstack as
  `logkeeper`'s was. `logkeeper` bound its port again and connects were still
  refused for about 6 s (the line under "What the restored chain still
  owes"); what held the bound listener was never named, and sshserver binds
  again through a swapped netstack the same way.
- The file that authorizes a login. No test at any tier logs in over
  sshserver or reaches `authorized_keys`: its host tests call
  `authorizes(text, key)` and none calls `is_authorized`,
  `authorized_key_count` or `authorized_keys()`; no guest starts the daemon;
  and the T14's `metalcase` boot, read at `25766b613`, leaves at `no network on
  this machine` before the read. The reader that lost its second file is
  carried by the diff alone.

What reads the card next is not these rows restored. The owner ruled that the
rows judged from the stick alone — the T14 reaching its router and the
internet — are built on ToyOS's own network stack ("no smoltcp."), so they
arrive with `issues/toyos-has-its-own-network-stack.md`, which owns them.

## What the restored chain still owes

Each was an issue of its own and is true of `809c33c0c`'s code, so it comes
back with it:

- A swap on the T14 lives inside a metal boot's bound: every metal image
  carries `boot-deadline=` (`toyos_tco::wedge_bound_ms` of its list's bound)
  and a runner whose list ends in `reboot` under that bound
  (`toyos_tco::list_bound_ms`), so the loop the owner
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
- A netstack that dies while serving leaves the host's stream silent: its
  connections go with no FIN and no reset, and only a swap the supervisor
  accepted was announced. Exit: a host reader learns of an unannounced death
  through an event, with a test whose replacement serves and then ends inside
  probation.
- Connects to `logkeeper`'s port were turned away for about 6 s after it had
  bound the port again and the supervisor had said `in service` (PR #566's
  `7e06a657`: bound at 1.758 s, in service at 6.739 s, admitted at 7.721 s).
  Exit: what holds a bound listener from accepting is named, and removed or
  bounded.
- A network reader that never reads holds one of `MAX_NETWORK_READERS` slots
  until the pipe, netstack's send buffer and the peer's window are full. Exit:
  it is let go on a bound that does not depend on how fast the log grows.
- The ssh client is no second implementation: its lock pins the `russh` commit
  `Cargo.lock` pins for sshserver, so a defect in the fork's protocol code is
  on both ends of every exchange.

Owner: the orchestrator, which holds the T14.

**Exit**: a connection the development host opens to the T14 while it runs
ToyOS is answered, read as `lan_talk`, restored from `809c33c0c`, green from
the T14, and each line of "What the restored chain still owes" is fixed or
filed again as its own issue in that restore.
