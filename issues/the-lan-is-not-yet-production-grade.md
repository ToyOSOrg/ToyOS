---
status: open
kind: track
opened: 2026-09-23
---

# The LAN reaches a router, and is not yet production grade

The T14's onboard I219 (00:1f.6, `8086:15fc`) leased an address from a router
this project does not control: bench run 113, `exit: netd pid=5 code=83`,
`leased 192.168.1.49/24 from 192.168.1.1`, over netd's own driver, MSI, its own
IOMMU domain and its own stack. The bar is a gigabit driver nobody has to
apologise for on any modern machine, and that is four stages away. Each stage
has its own exit, and a stage lands whole rather than in slices.

## 1. Consolidation

The bring-up was found by scouting, and a found path carries its scaffolding.

- Every scout arm and instrument the path no longer needs is deleted with the
  issues filed for it; the crumb trail stays only while a question needs it.
- The wake sequence is reduced to what the hardware requires. Run 112 climbed
  several steps and only forcing SMBus and returning to the PCIe-encoded
  interconnect woke the PHY: each remaining step is justified by a cited
  hardware property or removed.
- The 100 ms pace between arbitration accesses rests on one machine death
  (run 100) and one survival (run 101). It is measured down or explained.
- The MAC and PHY bring-up is one driver path, not the layers the scouting left.

**Exit**: the path is the smallest one the hardware accepts, every step cites
why, and the boot still leases.

## 2. A gigabit driver that holds

- **1000 Mb/s.** Done: bench run 124 linked at 1000 full.
- **Link events** arrive by interrupt and are recovered from without a restart:
  cable pulled and replugged, the partner rebooting, a flapping link.
  Owed for it: a link partner a T14 row commands, so a row can drop the link
  and raise it under traffic. Nothing has one, and
  `issues/no-machine-has-read-an-intel-nic-across-a-link-change.md` waits on it.
- **Throughput** is measured against Ubuntu on the same machine and cable, with
  line rate (about 940 Mb/s of TCP) as the target: interrupt moderation (a
  fixed throttle today, not adaptive), ring sizes, checksum and
  segmentation offload.
- **The part is handed back** in the state the next operating system and the
  management firmware that shares it expect. Ubuntu has had link after every
  ToyOS boot so far; that becomes a tested property.
- **Every received frame is hostile.** The receive path and every parser above
  it are fuzzed; a malformed frame ends netd at worst, never the kernel.
- **Error counters** (CRC, missed, no-buffer) are read and asserted zero under load.
- **No register is left to its reset value, but `TIDV` and the receive
  addresses past the first, deliberately.** Done for these registers (pull
  request #1078). The T14's reset does not give
  82574 §10.2.6.1's `TCTL`, which the bring-up now writes whole (pull
  request #1075). It now also sets `TARC0`'s `ENABLE` (bit 10), which
  §10.2.6.9 says "should always be set", as one bit, since the PCH MAC's
  other `TARC` bits are unpublished; and writes `RXCSUM` (§10.2.5.15),
  `MRQC` (§10.2.5.25) and `RFCTL` (§10.2.5.16) to their reset values but
  `RXCSUM`'s two checksum offloads. `TIDV` is not written: §10.2.6.10 allows
  no 0, and it delays only a descriptor with `IDE` set, which the driver
  sets on none. The receive addresses past the first are left alone, the
  orchestrator's decision: the driver programs entry 0 alone. On the 82574
  a master reset clears `AV` in entries 0-14 and "Entry 15 is reserved for
  manageability firmware usage" (§10.2.5.23); on the I219 "up to eleven"
  shared addresses "can be allocated to manageability" (612523
  §7.4.2.3.1), at offsets no PCH document publishes, so their valid bits
  are read at the 82574's. On the T14, out of the reset ([evidence
  `800e35c69/`](https://github.com/ToyOSOrg/evidence/tree/a911b388eac3990bde439b95091b41a00335748c/r1/800e35c69)): `TXDCTL` `0x00000000`, `TARC0` `0x00000403`,
  `RXCSUM` `0x00000300`, `MRQC` and `RFCTL` `0x00000000`, each a value the
  82574 documents, and no receive address past the first valid.

**Exit**: 1000 full, line-rate TCP within a stated margin of Ubuntu's, link
survives unplug and replug, the handback is tested, the fuzzers run in CI.

## 3. One driver per device, one stack

Built as P4 of `issues/there-is-no-wifi.md`, which holds what of its exit the
benches still owe.

- One driver process per network function, claiming exactly that function:
  each driver a `drives` row (`system/i219`, `system/virtio-net`).
- netstack is the stack alone, and meets every driver through one narrow
  interface (`toyos-link`): frames over a shared region's rings, and the link
  state, speed and MAC address. No MTU or offload is in it: the stack sends
  frames of 1,514 bytes at most, and no driver offers an offload.
- The supervisor binds drivers to hardware from the `drives` rows' match
  table by vendor, device and class, instead of a row naming the card.

**Exit**: killing a driver process restarts it with the stack's connections
intact, and a machine with an unlisted but matching NIC comes up without a
config change.

## 4. The stack

DHCP leases today. Production grade is DHCP renewal and expiry, ARP and
neighbour handling, IPv6 (the bench's network carries it), TCP that behaves
under loss (retransmission, window scaling, congestion control), DNS, and
interoperability with peers other than this project's own tests.

**Exit**: a stated set of RFC behaviours, each with a test, and a soak of hours
of real traffic with no error, leak or stall.

## Standing

The bench tests that prove each exit run in the T14 suite on every change to
the path, not once. Today none does: no T14 row reads the wired card
(`issues/the-host-cannot-reach-the-t14-while-it-runs-toyos.md`). The bench itself stays honest: the boot stick's first
command still sometimes goes unanswered on the USB2 half of its receptacle
(runs 84, 103, 104, 105), and everything above rests on the record it carries.
