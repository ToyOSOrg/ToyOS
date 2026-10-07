---
status: open
kind: tooling
opened: 2026-10-07
---

# No T14 row reads an ACPI_DISABLE asked off the boot processor

`smi_cmd::write` (`kernel/src/arch/x86_64/smi_cmd.rs`) makes every write to
`SMI_CMD` on the boot processor: a caller on another CPU kicks it and spins
until it has written. The T14's `acpi_server_death` row reads that crossing
for the enable, because its job claims from a thread it found off the boot
processor and the judge refuses `asked from cpu0` there. Two things it does
not read:

- **The disable asked from another CPU.** `acpi_mode::release` runs in the
  task that drops the claim's last handle. On the `acpicase` boot that is the
  killed `/system/bin/acpiserver`'s last thread, in its teardown and under
  its process's lock (`process::teardown_resources`), on the CPU the kill
  found it on: a killed task is never migrated. The job places no thread of
  another process, and the tree has no affinity, so where the server dies is
  the scheduler's.
- **The boot processor's answer from a lock's spin.** `tlb::poll` answers a
  round for a boot processor that spins with interrupts closed on a lock the
  asker may hold, where no kick reaches it. No row arranges that contention,
  and the kernel's line does not say whether a write was answered from the
  kick or from a spin.

The recorded `acpicase` boots, ten distinct ones: the `legacy mode again`
line was logged on cpu0 in nine and on cpu1 in one, an early head of the
stage that added the claim, whose line named no asker. The one boot of a
kernel whose line names it, `448107b54`, reads `ACPI_DISABLE 0xf1 written to
SMI_CMD 0xb2 on cpu0, asked from cpu0`. So the wait under the dying process's
lock has run on no machine, and neither has the spin's answer.

What could arrange the first, unbuilt and unmeasured: a holder that reads
itself off the boot processor and ends holding the claim, so its own teardown
is the release; or a thread of the job's kept running on the boot processor
from before the server's spawn to after its kill, so that no placement puts
the server there, which rests on where a wake puts a parked thread and that
has not been read against it.

Owned by `issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`.

**Exit**: a T14 row's judge refuses a boot whose `legacy mode again` line
reads `asked from cpu0`, on a job that arranges the asker and does not leave
it to the scheduler; and a T14 row reads a kernel line saying a write to
`SMI_CMD` was answered from a lock's spin.
