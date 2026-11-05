---
status: open
kind: tooling
opened: 2026-10-07
---

# No T14 row arranges an ACPI_DISABLE asked off the boot processor

`smi_cmd::write` (`kernel/src/arch/x86_64/smi_cmd.rs`) makes every write to
`SMI_CMD` on the boot processor: a caller on another CPU kicks it and spins
until it has written. The T14's `acpi_server_death` row reads that crossing
for the enable: its job claims from a thread held to the last CPU
(`tests/guest/rust-tests/src/off_the_boot_processor.rs`), and the judge
refuses `asked from cpu0` there. Two things it leaves to the scheduler or
does not read:

- **The disable asked from another CPU.** `acpi_mode::release` runs in the
  task that drops the claim's last handle. On the `acpicase` boot that is the
  killed `/system/bin/acpiserver`'s last thread, in its teardown and under
  its process's lock (`process::teardown_resources`), on the CPU the kill
  found it on: a killed task is never migrated. The job places no thread of
  another process, and nothing pins the server, so where it dies is the
  scheduler's. The judge prints the disable's asker and requires nothing
  of it: a judge that required a crossing no job arranges would be a flake.
- **The boot processor's answer from a lock's spin.** `tlb::poll` answers a
  round for a boot processor that spins with interrupts closed on a lock the
  asker may hold, where no kick reaches it. No row arranges that contention,
  and the kernel's line does not say whether a write was answered from the
  kick or from a spin.

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

What could arrange the disable's asker, unbuilt and unmeasured: the server
spawned from the job's held thread, which starts with that thread's mask
(`toyos_abi::syscall::thread_set_affinity`), so its threads, and the
teardown its kill ends in, run on that CPU alone.

Owned by `issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`.

**Exit**: a T14 row's judge refuses a boot whose `legacy mode again` line
reads `asked from cpu0`, on a job that arranges the asker and does not leave
it to the scheduler; and a T14 row reads a kernel line saying a write to
`SMI_CMD` was answered from a lock's spin.
