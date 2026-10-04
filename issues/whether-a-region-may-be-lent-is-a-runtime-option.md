---
status: open
kind: defect
opened: 2026-09-26
---

# Whether a region may be lent is a runtime `Option`

`SharedMemObject::ram` (`kernel/src/object/shm.rs`) decides whether a device
may be lent a region by reading `Region::pages` — `Some` for pages the kernel
allocated for the region, `None` for an aperture, firmware's framebuffer, or a
pool a kernel driver owns — together with its cache policy. A new region kind
that fills the field the wrong way is lendable, or refused, with no compile
error; the only check is `blockd_lends_within_its_bound`'s refusal of
virtio-sound's pool. And the field's name says nothing about lending: the
virtio-gpu scanout and cursor carry `Some` and are lendable today, which is the
same authority their holder already has but was decided by nobody.

**Exit condition.** A region's kind is a type — owned pages, a kernel driver's
pool, an aperture — and `dma_map` takes only the owned-pages kind, so a region
that may not be lent cannot be passed to it.
