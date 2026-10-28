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
- **No register is left to its reset value.** The T14's reset leaves `TCTL`
  at the value it held before it, not §10.2.6.1's initial values (pull
  request #1075, which now writes every `TCTL` field). The bring-up still
  relies on whatever a reset leaves in registers it never writes: `TARC0`,
  whose `ENABLE` (bit 10) "should always be set" (82574 §10.2.6.9; set as a
  bit, since the PCH MAC's other `TARC` bits are unpublished, with the
  `TXDCTL` change of
  `issues/the-intel-driver-writes-txdctl-and-tidv-against-the-82574-datasheets-newer-revision.md`);
  `RXCSUM` (§10.2.5.15), `MRQC` (§10.2.5.25), `RFCTL` (§10.2.5.16), and the
  receive address entries `RAL`/`RAH` 1 and up. `regs.rs`'s `FRAME_IS_BAD`
  leaves `IPE` and `TCPE` out because "this driver asks for none" receive
  checksum offload, which is false even at the documented reset: §10.2.5.15
  gives `RXCSUM.IPOFLD` and `TUOFLD` an initial value of `1b`. Ignoring the
  two bits costs nothing; the reason given is wrong. Exit: the bring-up
  writes or sets each of these to a value it chose, a T14 boot log says
  their values out of the reset, and the comment's reason is the one that
  holds.

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
the path, not once. Today none does: no T14 row reads the wired card
(`issues/the-host-cannot-reach-the-t14-while-it-runs-toyos.md`). The bench itself stays honest: the boot stick's first
command still sometimes goes unanswered on the USB2 half of its receptacle
(runs 84, 103, 104, 105), and everything above rests on the record it carries.
