---
status: open
kind: tooling
opened: 2026-09-27
---

# The bench has no quiescent log volume for the outside judge to read

`toyos-fat32-check` judges the log partition's own bytes, read off the stick
while nothing writes it — Ubuntu, between two ToyOS boots. On the bench the
machine that comes back is ToyOS with that volume mounted and `logd` appending
to it, so no read of it over ssh is a volume at rest, and `toyos-metal` refuses
`--fat32-check` without `--via-ubuntu`. Every metal boot delivered to the bench
is judged without the one reader of those bytes that is not the family of code
that wrote them.

**Exit**: the bench path reads the log volume at rest — a pass of the loader,
which runs before anything mounts it, or a boot that holds it unmounted — and
`--fat32-check` runs on every bench boot again.
