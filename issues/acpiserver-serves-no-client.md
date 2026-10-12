---
status: open
kind: defect
opened: 2026-10-12
---

# acpiserver serves no client, so no driver can read its own device's ACPI methods

`[programs.acpiserver]` in `images/system.toml` serves nothing: it holds the
`acpi` claim and receives `power`. Its module doc (`system/acpiserver/src/main.rs`)
names everything it evaluates: the fixed hardware, the embedded controller,
and the batteries and AC adapters.

A userland driver whose device keeps something in the ACPI namespace has no
way to reach it. The Wi-Fi driver needs the per-device transmit power table
that the platform keeps under the radio's ACPI device, before it sends its
first frame (#876 §4, P8).

Owner: P8 of `issues/there-is-no-wifi.md`.

**Exit**:
- acpiserver evaluates the methods under one PCI function's ACPI device for
  that function's driver alone, over a connection the supervisor opens for
  that binding;
- nothing names another device's scope;
- host tests hold the binding's refusals;
- a T14 row reads the AX210's tables.
