---
status: open
kind: defect
opened: 2026-10-02
---

# Two submitters of one ring race its last completion slot

`polls::deliver` asks the ring for room and then answers, in two holds of the
completions' lock. Two threads in `inbox_submit` on one ring can both find the
last slot free; the second answer finds the ring full, and `post_completion`
drops it and counts it in `dropped`, as it does every completion written to a
full ring. A ring one thread submits to drops no watch's answer, and
`toyos::Poller` sizes its rings past what its watches can answer.

**Exit**: the room an answer was looked up for is the room it is written
into; a test runs two submitters against a ring with one slot.
