---
status: open
kind: defect
opened: 2026-11-05
---

# `acpi_press_is_one_sci` boots to a console that carries no line past the UART's hand-over

`acpi_press_is_one_sci` (`tests/images/acpicase.toml` with the probe's
`press` arm, `Profile::HeadlessNoConsolePort`, the shipping kernel) stalls
on every run since the UART became its driver's (#1175): `STALLED: waiting
for the probe to arm the power button — it went quiet`. The console carries
the boot through the kernel's `boot: power-on to loader … kernel to Boot:
complete` and then nothing for the 17 s the harness waits: no supervisor
line, no `spawn:`, nothing of the userland the UART's driver carries. The
harness presses the button only on the probe's armed line, so the boot never
ends.

Runs, all `guest-rows.yml`, x86-64 KVM:
- 42010705757 and 42016111039 (`--nocapture`), #1187's head 0383550b0;
- 42025888487 (`--nocapture`), main at b27dd3e2.

Three of three, on main too, so it is not #1187's. The nightly of 11-03 (41618038084),
before #1175, passed the row on its kvm lane; the nightly has not run since.

**What the same runs establish beside it.** The other four acpicase rows
boot the same image on the same profile, and their consoles carry the
driver's lines after the hand-over and end in the stop: but each of those
boots ends within about half a second of `Boot: complete`. In 42010705757 the
console-port boot of `acpi_holder_is_refused_a_held_functions_configuration`
(its `uart-4.log`) carried the driver's lines to 2.139 s, stopped mid-line,
and carried nothing more until the kernel took the UART back for the stop at
2.544 s, whose replay then started past records the UART never showed. So a
boot that does not end soon is the one that shows it: whether the UART's
driver stops transmitting, `logkeeper` stops feeding it, or userland itself
stops is not read. None of the three stalls left a serial file
(`issues/a-red-run-keeps-no-serial-for-a-boot-whose-uart-is-qemus-stdio.md`),
and nothing read the guest's CPUs: the next step is the row with QMP's `info
registers -a` and the trace diary, or a boot of acpicase held open by a job
that sleeps.

Owner: the console UART's driver (`system/uart`) and its feed
(`system/logkeeper/src/carrier.rs`); held by the orchestrator.

**Exit**: the cause named and fixed, and `acpi_press_is_one_sci` green.
