---
status: open
kind: defect
opened: 2026-09-02
---

# Two refusal gates read the refusal line and never ask whether the device bound

`hda_two_live_refused` (`tests/common/hda.rs`) and `driver_wait_refused`
(`tests/toyos.rs`) both certify that the kernel refused a device by name. Neither
asks the machine afterwards whether the device is there. A kernel that prints
both refusals and still binds one controller, or still exposes the stuck NVMe and
virtio devices, passes — and `driver_wait_refused`'s own success line then says
"the boot came up without them" about a boot that did not.

`hda_two_live_refused` currently rests on `must_not_say("bound, statests=")`, one
line's spelling, plus soundd's null-sink announcement, which says what soundd was
given rather than what the kernel holds.

**The oracle exists and is not wired to either.** `kernel/src/device.rs`'s
`try_claim` answers `ClaimError::Absent` for `DeviceType::HdaAudio` exactly when
`drivers::hda::info()` is `None`, and for `DeviceType::Nic` exactly when
`net::nic_info()` is `None` — a fact no line rename can fake. Asking it needs a
guest binary on those two boots; `tests/toyos-rust-tests/src/bin/input_absent.rs`
is that probe for `Keyboard` and `Mouse` and is the shape to copy.

**Exit condition:** both gates run a guest arm that requires the refused class to
answer `NotFound`, with a positive control on a boot where the device is present.
The cost is one new registered name, which `tests/CLAUDE.md` prices at two CI
cycles.
