---
status: open
kind: tooling
opened: 2026-09-02
---

# `driver_wait_refused` cannot ask whether the stuck NVMe bound, because no class names one

`driver_wait_refused` (`tests/toyos.rs`) boots with `nvme-rdy-stuck` and
`virtio-reset-stuck`, reads the two refusal lines off the console and reports
"the boot came up without them". Nothing asks the machine whether either device
is there. A kernel that names both refusals and still exposes the stuck NVMe
passes, and the success line says the opposite.

**For the virtio half there is an oracle and it is simply not wired**:
`kernel/src/device.rs`'s `try_claim` answers `ClaimError::Absent` for a
`pci:<vendor>:<device>` request naming a function this machine does not have,
and `/system/bin/init` prints `init: <program>: no <device> on this machine
(<err>)` per refused claim (`userland/init/src/main.rs`). This boot's config
claims no NIC, so that line is not on its console and reaching it means giving
the config a claimant.

**For the NVMe half the oracle does not exist.** `DeviceType` has seven variants
and none of them is a block device:

    $ rg -n "^ *[A-Za-z]+ = [0-9]+ =>" toyos-abi/src/syscall.rs
    Keyboard = 0 => "keyboard",
    Mouse = 1 => "mouse",
    Framebuffer = 2 => "framebuffer",
    HdaAudio = 5 => "hda-audio",
    VirtioSound = 6 => "virtio-sound",
    PciFunction = 7 => "pci",

(3 and 4 are retired.) `PciFunction` is not the answer either: it names one
function by vendor and device id and hands it to a process to drive, and what
this gate has to ask is whether a controller *the kernel* binds bound. So no
`SYS_DEVICE_CLAIM` can answer for the stuck controller, and
the only thing that changes when a block device binds is downstream — a mount,
a `/home`, a `NVMe: block device id=` line — all of which are the same class of
console evidence the gate already rests on.

## Exit condition

Either a class that names a block device, so the claim table can be asked, or a
different instrument that reports the machine's bound block devices to a guest.
Then `driver_wait_refused` requires both stuck devices absent rather than
requiring only that they were named.

The sibling gate `hda_two_live_refused` is **not** in this record: init claims
`hda-audio` before it spawns soundd, and soundd reaches the null sink only where
that endowment is missing, so its existing `must_say(NULL_SINK)` already requires
`try_claim(HdaAudio)` to have answered `Absent`. That transitivity is now stated
at the assertion in `tests/common/hda.rs`.
