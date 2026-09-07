---
status: open
kind: track
opened: 2026-09-07
---

# The T14 has a wired NIC and nothing drives it, so every test result waits for a flash

The T14 is on a cable on the same LAN as the development Mac (owner, 2026-09-07),
and its NIC is the onboard Intel I219 at `00:1f.6`, `8086:15fc`, which the kernel
enumerates and nothing claims. The only channel off the machine is the log
partition on the stick, read after the machine has rebooted into Ubuntu
(`issues/hardware/the-t14-boots-toyos-unattended.md`): a verdict costs a flash, a
boot and a return, a hang shows nothing until the power is cut, and nothing a
program prints reaches the harness at all — the reason 163 registrations stay on
QEMU.

What is to be built, in this order:

- **A kernel driver for the I219** behind the `nic` device netd already claims
  (`userland/netd/src/main.rs`'s `NicDev`: `rx_poll`, `rx_done`, `tx`), so netd
  and smoltcp run unchanged on the laptop. It needs no firmware; the datasheet is
  the oracle and `tests/netcase` on the T14 is the first judge. Its DMA goes
  through a device domain like the NVMe's, never the identity domain.
- **A record stream**: logd gains a network sink that sends every record to a
  listener on the Mac as it is written, so the harness reads the boot live and a
  hang is visible at its last line. The black box and the stick stay the channel
  for boot and panic, which no daemon can report.
- **ssh into the T14 from the harness**, over the sshd the tree has, for running
  a test and reading its output without a flash — the second half of
  `issues/build/there-is-no-network-gate.md`, which certifies both NICs with
  the same guest-side tests.

Constraints a reader would otherwise re-derive: Wi-Fi is not this track — the
AX210 needs a firmware image the dependency rule refuses
(`issues/hardware/there-is-no-wifi.md`). A kernel change still costs a flash
until the loader is fetched over the network; this firmware's HTTP boot has not
been tried, and if it works the stick leaves the boot path with the wedged-stick
failure class. Sequenced after the metal suite lands.
