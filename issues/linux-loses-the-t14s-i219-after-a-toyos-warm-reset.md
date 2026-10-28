---
status: open
kind: defect
opened: 2026-10-28
---

# Linux loses the T14's I219 after a ToyOS warm reset

On 2026-10-28 a T14 boot (Tiger Lake PCH, I219-V `8086:15fc`) ran netstack's
I219 driver, linked at 1000 Mb/s, and rebooted at 201 s through the FADT's
reset register, `0xCF9` ← `06h`: a host reset without a power cycle, which
keeps the PHY powered. Linux's e1000e then logged "Failed to disable ULP" and
its probe failed with -2, a PHY that answered on neither PCIe nor SMBus even
after a `LANPHYPC` power cycle. The NIC stayed absent until AC power was
removed. The same image had booted into Linux cleanly five minutes earlier.
ToyOS's log of that boot is the stick's log partition, kept by the
orchestrator; the failing Linux boot is the T14's journal at 10:51:19 UTC.
ToyOS's own start found the PHY as on every logged boot: silent over PCIe,
silent after its power cycle, answering once the MAC was forced onto SMBus.

**Candidates, none shown:**

- (a) An Ultra Low Power configuration ToyOS never cleared, armed by the agent
  before it, carried through the host reset into Linux. #1091 disarms it at
  start and logs what it found.
- (b) PHY state left by ToyOS's own reach for the PHY: the `LANPHYPC` hold
  was 100 ms before #1091 (now at least 10 µs, released at once), and its PHY
  transactions follow forcing SMBus.
- (c) A PHY that failed on its own.

**For (a): the bench's Linux has magic-packet wake on.** Before the T14 boot
of 2026-10-28 13:22 UTC its e1000e reported "Supports Wake-on: pumbg" and
"Wake-on: g". A Linux driver arms ULP at its own shutdown, reboot and kexec when
a wake is wanted and none of the filters ULP cannot serve (unicast, multicast,
broadcast) is among them, and magic packet is not one of those. So every
Linux reboot into ToyOS on this bench leaves ULP armed, which fits ToyOS's start
finding the PHY silent over PCIe and answering only on SMBus on every logged
boot. Before #1091 nothing in ToyOS cleared it.

**One cut-off trial.** That same boot, an `i219_leaves_ulp_unarmed` image with no
job of its own at `a6ae98b35`, rebooted 3 ms after netstack started, so the
driver was cut off part way through its wake. Linux found the NIC afterwards.

**Instrument.** Since #1091 the wake's line says what ULP held when the PHY
first answered, and the bring-up's what it holds after the reset. Every T14
metal boot ends with `0xCF9` ← `06h` into the loader's chain pass and the
bench's Linux, and the orchestrator's runner checks e1000e's probe after each
one: each boot is one trial of the failing transition. The trials are
`nic-trials.tsv` on the evidence repository's `bench-nic-trials` branch (date,
run label, revision, `LINUX-NIC` result), appended after each boot; that file
is this issue's trial count.

**K1, not settled.** The disarm enables K1 (page 770 register 17 bit 14), as
the vendor's disable does. Whether this PCH needs K1 off at gigabit is outside
what the clean-room facts cover; it is a question for the fact half, and a
link fault at gigabit after #1091 points here first.

**Owner**: the I219 driver (`system/netstack/i219/`).

**Exit**, in order:

1. The bench's Linux, its Wake-on-LAN setting recorded, reboots into a ToyOS
   image carrying the instrument: the wake's reading is what Linux and the
   firmware left.
2. That image reboots into itself: the wake's reading is what ToyOS left
   across a host reset.
3. The image reboots into Linux, and e1000e's probe outcome from its log goes
   in evidence (Linux is a black-box measurement here, not a committed
   oracle).

Steps 1 and 2 settle candidate (a). A clean step 3 alone proves nothing for a
failure seen once: calling the defect fixed needs the base image's failure
rate measured first, or, at a rate near the one in 176 logged boots, about
500 clean cycles for 95% confidence.
