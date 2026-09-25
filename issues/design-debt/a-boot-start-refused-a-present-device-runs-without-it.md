---
status: owner
kind: question
opened: 2026-09-24
---

# A boot start refused a device the machine has runs without it

`/system/bin/init`'s `start` mints every device a `[programs]` row names. At a
swap or the restart that rolls one back, a device the process being replaced
held and the new one is refused fails the start (`Served::Restart`'s `owed`).
At boot there is no process before it, and a refusal is said in the kernel's
own word (`init: netd: pci:8086:10c9 is on this machine and could not be
handed over`) and the program is started without that device.

That is not the swap's defect — init claims nothing about a boot start beyond
`started`, and a row names every device its program can drive, so a machine
with a subset is a configuration, not a fault — but a refusal other than
`NotFound` is a fault, and the program then runs on less than the machine
has. `tests/common/faults.rs`'s `refused_claim` and `pci_function_is_exclusive`
assert exactly this behaviour today.

The question for the owner: should a boot start refused a device the machine
has — `NotSupported`, `AlreadyExists`, `ResourceExhausted` — be fatal? A failed
boot start is an init panic, so the machine would not boot over one device;
and a row cannot yet say which of its devices its program needs.
