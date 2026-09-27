---
status: open
kind: defect
opened: 2026-09-27
---

# A loader change reaches a machine only by writing its stick again

`update` installs a slot — kernel, boot parameter, ROOT — and never the loader
on the ESP, so a machine keeps the loader it was flashed with while every
kernel after it arrives by update. Nothing holds the two together: a kernel
built against a changed `KernelArgs`, black box or slot record boots under the
old loader as if it matched.

The bench refuses it for the images it judges: its loader names itself by its
file's hash in `loader.log` (`LOADER_IS`), and `toyos-metal` delivers nothing
whose own loader differs (`metalbench::same_loader`), so a loader change costs
the T14 a new bench written through `--via-ubuntu --resident`. That path goes
with the installer, and an owner's machine updated with `ssh … update` has no
check at all.

**Exit**: a loader change reaches a running machine through a signed update,
or an image names the loader it needs and a loader that is not it refuses to
boot it by name.
