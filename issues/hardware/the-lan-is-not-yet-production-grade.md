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

- **1000 Mb/s.** The part links at 10 full where Ubuntu gets 1000 full on the
  same cable; its mechanism is filed with the PHY branch, PR #453.
- **Link events** arrive by interrupt and are recovered from without a restart:
  cable pulled and replugged, the partner rebooting, a flapping link.
- **Throughput** is measured against Ubuntu on the same machine and cable, with
  line rate (about 940 Mb/s of TCP) as the target: interrupt moderation (off
  today), ring sizes, checksum and segmentation offload.
- **The part is handed back** in the state the next operating system and the
  management firmware that shares it expect. Ubuntu has had link after every
  ToyOS boot so far; that becomes a tested property.
- **Every received frame is hostile.** The receive path and every parser above
  it are fuzzed; a malformed frame ends netd at worst, never the kernel.
- **Error counters** (CRC, missed, no-buffer) are read and asserted zero under load.

**Exit**: 1000 full, line-rate TCP within a stated margin of Ubuntu's, link
survives unplug and replug, the handback is tested, the fuzzers run in CI.

## 3. One driver per device, one stack

netd holds every NIC driver and the stack in one process, and `system.toml`
names the card per machine (`devices = ["pci:1af4:1041"]`). Neither scales to
all modern hardware: a bug in one driver takes every connection with it, a
driver cannot restart without the stack's state, and a machine nobody has seen
has no row.

- One driver process per network function, claiming exactly that function.
- netd is the stack alone, and meets every driver through one narrow interface:
  frames over shared-memory rings, link state, speed, MAC address, MTU, offloads.
- init binds drivers to hardware from a declared match table by vendor, device
  and class, instead of a row naming the card.

The drivers already live in their own crates (`toyos-i219`, the virtio driver),
so this is a move of hosts, not a rewrite. It lands before a second real NIC
family is added, which is when the monolith starts to cost.

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
the path, not once. The bench itself stays honest: the boot stick's first
command still sometimes goes unanswered on the USB2 half of its receptacle
(runs 84, 103, 104, 105), and everything above rests on the record it carries.
