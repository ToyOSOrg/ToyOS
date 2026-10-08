---
status: open
kind: tooling
opened: 2026-10-07
---

# No T14 row arranges an ACPI_DISABLE asked off the boot processor

`smi_cmd::write` (`kernel/src/arch/x86_64/smi_cmd.rs`) makes every write to
`SMI_CMD` on the boot processor: a caller on another CPU kicks it and spins
until it has written. The T14's `acpi_server_death` row reads that crossing
for the enable, because its job claims from a thread it found off the boot
processor and the judge refuses `asked from cpu0` there. Three things it
leaves to the scheduler or does not read:

- **The disable asked from another CPU.** `acpi_mode::release` runs in the
  task that drops the claim's last handle. On the `acpicase` boot that is the
  killed `/system/bin/acpiserver`'s last thread, in its teardown and under
  its process's lock (`process::teardown_resources`), on the CPU the kill
  found it on: a killed task is never migrated. The job places no thread of
  another process, and the tree has no affinity, so where the server dies is
  the scheduler's. The judge prints the disable's asker and requires nothing
  of it: a judge that required a crossing no job arranges would be a flake.
- **The boot processor's answer from a lock's spin.** `tlb::poll` answers a
  round for a boot processor that spins with interrupts closed on a lock the
  asker may hold, where no kick reaches it. No row arranges that contention,
  and the kernel's line does not say whether a write was answered from the
  kick or from a spin.
- **The enable's arrangement has a window.** The job's claiming thread reads
  its x2APIC id and then enters the kernel
  (`tests/toyos-rust-tests/src/bin/acpi_release.rs`,
  `claim_off_the_boot_processor`). The kernel moves no running thread, so the
  asker is the CPU the thread read, unless a preemption between that read and
  the kernel's lock in `smi_cmd::write` queues the thread behind another and
  an idle boot processor takes it. With no affinity in the tree the window
  cannot be closed.

The disable, on the recorded `acpicase` boots. Ten before `289906d5b`: the
`legacy mode again` line was logged on cpu0 in nine and on cpu1 in one, an
early head of the stage that added the claim, whose line named no asker; the
one of the ten whose line names it, `448107b54`, reads `on cpu0, asked from
cpu0`. `289906d5b` reads `ACPI_DISABLE 0xf1 written to SMI_CMD 0xb2 on cpu0,
asked from cpu2`, and the same head with the read of the CPU removed from
`answer` reads `on cpu2, asked from cpu2`. So the wait under the dying
process's lock has run on the machine once, by the scheduler's doing and not
the job's: of the three boots whose line names the asker, one did not cross.
Whether any write on any boot was answered from a lock's spin is not known:
no line says which path answered.

The window, on those two boots, the only ones of the job that places its
claim: neither hit it. Each enable reads `asked from cpu1` beside the job's `acpi_release: claimed from
the CPU of x2APIC id 2`. A boot that does hit it reds the row as

`FAIL acpi_server_death: the job's claim was asked from the boot processor, so no CPU asked it for this write:`

over an enable line reading `on cpu0, asked from cpu0`, while the job's own
line names a non-zero x2APIC id. That pair is this window and no defect of
the kernel's write: it is answered here, by closing the window or by the
row's judge giving up the refusal, and never by a second boot.

What could arrange the disable's asker, unbuilt and unmeasured: a holder that
reads itself off the boot processor and ends holding the claim, so its own
teardown is the release; or a thread of the job's kept running on the boot
processor from before the server's spawn to after its kill, so that no
placement puts the server there, which rests on where a wake puts a parked
thread and that has not been read against it. Either inherits the window
above.

Owned by `issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`.

**Exit**: a T14 row's judge refuses a boot whose `legacy mode again` line
reads `asked from cpu0`, on a job that arranges the asker and does not leave
it to the scheduler; a T14 row reads a kernel line saying a write to
`SMI_CMD` was answered from a lock's spin; and the thread whose asker a judge
refuses is held to its CPU from before it reads where it is until the kernel
has taken its request.
