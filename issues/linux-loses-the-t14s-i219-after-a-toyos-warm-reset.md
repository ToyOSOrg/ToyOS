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

**What the T14 read** (2026-10-28 13:37 UTC, `2fb6b8c60`, booted from the
bench's Linux with `Wake-on: g`; row `i219_leaves_ulp_unarmed` passed and
Linux found the NIC afterwards):

- The wake found ULP Configuration 1 at `0x1800`: bits 11 and 12, the two that
  keep sticky ULP and SMBus across `PERST#`, with sticky ULP, reset to SMBus,
  in-band exit and host wake clear and no power-up-from-ULP indication. It was
  read after ToyOS's own `LANPHYPC` cycle and its forcing of SMBus, so those
  two bits outlived Linux's shutdown, the host reset and a power cycle of the
  PHY by its pin. Power control read `0x9084` (K1 off) and `0xE4` read
  `0x80000000`, bit 5 already clear. `LANPHYPC` was held low for 10.09 µs.
- After the disarm and the reset of MAC and PHY together, the bring-up read
  ULP Configuration 1 at `0x0000`, power control `0x9084` again, `0xE4`
  unchanged.

So the premise of (a) is observed: Linux's reboot leaves an arming that
reaches the next start through everything ToyOS's start did before #1091,
which never cleared it. That it is what made e1000e fail is not shown: that
is the trial count's question.

**K1 reads off after the disarm.** The disarm wrote power control with bit 14
set (`0xD084`), and the next read, after `RST|PHY_RST`, is `0x9084`. Nothing
read the register between the write and the reset, so the readings cannot
tell a write the PHY ignored from a reset that returns the register to
`0x9084`. A read straight after the write tells them apart. K1's state at
gigabit was already the open question above; the disarm's ULP result does
not depend on it.

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

**Exit.** Step 1 of the original sequence, Linux with its Wake-on-LAN setting
recorded into an image carrying the instrument, is done (above). Step 2, the
image into itself, is no longer needed for (a): its premise is observed, and
what step 2 would add is that the fixed driver's `0x0000` survives a host
reset unarmed, which every trial's Linux probe after a fixed boot exercises
directly. What remains is step 3 repeated: each T14 boot's e1000e probe after
a ToyOS boot, in `nic-trials.tsv`. A clean trial alone proves nothing for a
failure seen once: calling the defect fixed needs the base image's failure
rate measured first, or, at a rate near the one in 176 logged boots, about
500 clean cycles for 95% confidence.
