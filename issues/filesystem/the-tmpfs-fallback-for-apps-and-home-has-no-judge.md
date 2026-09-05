---
status: open
kind: defect
opened: 2026-09-04
---

# Nothing asserts a machine with no DATA volume gets a working `/apps` and `/home`

## What is unjudged

`kernel/src/main.rs`'s `open_data()` answers `None` three ways — no TOYOS-DATA
partition, two of them, or a volume that is not ours — and every one lands on
one `TmpFs` mounted under both names:

    storage: /apps and /home are a tmpfs — they will not survive a reboot

Three tests read that line, and all three read it as a *negative premise*:
`tests/common/storage.rs:277`, `:366` and `:458` each refuse to continue if it
appears, because their readback would then judge no device. No test boots a
machine without a DATA volume and asserts the line is there, that both `/apps`
and `/home` are writable, and that the two are still one filesystem — which is
the whole of what the fallback promises. So the arm a first boot on unprepared
hardware takes is the arm nothing runs.

The shape it hides: the fallback mounts one `TmpFs` under `&DATA_PATHS`, so
`/home/x` is `home/x` and `/apps/y` is `apps/y` inside it. A fallback that
mounted two `TmpFs` instances, or one under a single name, would pass every
test in the suite.

## Reproduction

`Profile::Diskless` and `Profile::Metal8042Absent` both declare `nvme_bytes: 0`
(`tests/common/qemu.rs:1695`, `:1742`), which emits no controller, no namespace
and no backing file. Boot either and the line is printed; nothing then asks the
guest anything about the two paths.

## Exit condition

A machine test boots a `nvme_bytes: 0` profile, asserts the line, and runs a
guest binary that writes under `/apps` and under `/home`, reads both back, and
finds neither name answering for the other's file — the same four claims
`hierarchy_paths` makes against DATA, against the fallback instead. Closes when
that test is registered and green.
