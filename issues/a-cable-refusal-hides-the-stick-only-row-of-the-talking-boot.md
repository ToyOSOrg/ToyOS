---
status: open
kind: tooling
opened: 2026-10-08
---

# A cable refusal hides the stick-only row of the talking boot

`toyos-metal` refuses a talking boot whose host heard nothing over the cable,
and the harness takes a refused boot's readback as the refusal
(`read_readback`, `tests/common/metal.rs`): every row riding the boot is red
with the refusal's text, whatever it reads. `lantalkcase` carries three rows.
`lan_talk` reads the conversation and `lan_dhcp_lease` holds the lease to the
address the host reached, so both need the cable. `lan_message_delivery` reads
only the kernel's records on the stick, and is red with them.

## Measured

The T14, `lantalkcase`, with the bench's wired path down
(`issues/lan-talk-is-unread-on-the-t14-since-the-claims-binding-landed.md`), at
`main`'s `6f87cdb9c` and at `c84f5fafe` the same day. Both `verdict.txt`:

```
refused
the boot did not say over its own cable what a talking boot owes:
  the stream never opened
```

At `c84f5fafe` the judge printed the same line for all three rows:

```
FAIL lan_message_delivery: toyos-metal refused lantalkcase: the boot did not say over its own cable what a talking boot owes:
```

and the stick it did not read carries what that row asks for:

```
pcidev: PCI 00:1f.6 [8086:15fc] handed over on slot 0, vector 0x28
pcidev: slot 0 took its first message on vector 0x28
```

## Owner

The harness's author.

## Exit condition

On a `lantalkcase` readback refused only for what the host heard,
`lan_message_delivery` is judged on the stick's records, shown by a
`toyos-checks` fixture of such a readback.
