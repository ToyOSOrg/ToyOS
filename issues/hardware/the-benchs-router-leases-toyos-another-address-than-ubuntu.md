---
status: open
kind: tooling
opened: 2026-09-29
---

# The bench's router leases ToyOS another address than Ubuntu, so `lan_dhcp_lease` reds on the router

`lan::on_metal` (`tests/common/lan.rs`) pinged the address Ubuntu held on the
I219's MAC before the flash, and refused a boot whose lease was another one.
This run measured that premise false: the router gave ToyOS `.49` twice and Ubuntu
`.46`, on one MAC.

## Measured

The full T14 run of `main` at `7e151819`
(EXIT=1), boot `lancase`:

```
  [lan] leased 192.168.1.49/24 from 192.168.1.1 in 13315 ms, gateway 192.168.1.1, dns [194.230.55.96, 212.98.37.130]
  FAIL lan_dhcp_lease: 2 finding(s):
  this boot leased 192.168.1.49 and the host pinged 192.168.1.46, which the router hands this MAC under the operating system before it — so either something else answered or that server does not repeat a lease across the two
  nothing answered a ping at 192.168.1.46 while this machine was between its two operating systems
```

`lancase/boot.txt` records `ping_addr 192.168.1.46` and
`wire_mac 38:f3:ab:35:37:3b`; `netd: MAC 38:f3:ab:35:37:3b` is the MAC the
lease went to. The `lantalkcase` boot of the same run leased
`192.168.1.49` again (`talk_peer 192.168.1.49`). What the router keys the
lease on is not measured.

## Exit condition

`lan_dhcp_lease` rides `lantalkcase` and holds the lease to the address the
boot answered at, not to one Ubuntu held; a T14 run passes it, and then this
file is deleted.
