---
status: open
kind: tooling
opened: 2026-10-13
---

# A metal readback does not prove which image wrote its log

`read_readback` (`tests/common/metal.rs`) judges a readback directory's
`kernel.log` without holding the log's `supervisor: build <tree> clean` line
to the tree doing the judging, so a judge from one tree passes or reds a boot
of another's image and says nothing. And `--metal --metal-readback <dir>`
from a tree whose images the directory does not hold stages that tree's
images into it, overwriting the `image.img` that was booted, so the directory
no longer holds the image that wrote its log.

**Evidence.** #886's round-2 `t14-reseal` arm, booted from tree `b7a7b062a`
(image sha256 `c6d44fe8…`): a first judging from the head tree `e2b398cb6`
staged that tree's image beside the readback and judged nothing (exit 2,
https://github.com/ToyOSOrg/evidence/blob/f17cea3bdae1bda4586220fc7b9f551d9cecff49/r2/readings/t14-reseal/judge-from-e2b398cb6-tree.log);
the judging from `b7a7b062a` in a second checkout, over a copy of the
readback, rebuilt and wrote its `image.img` (`55bec413…`), never booted and
2,388,453 bytes from the booted one
(https://github.com/ToyOSOrg/ToyOS/pull/886#issuecomment-6135201083). That
the log came from `b7a7b062a` was told by reading its `supervisor: build`
line; no check did.

Owner: the orchestrator.

**Exit**: a judge from a tree other than the one a readback's log names
refuses the readback by name, and judging a readback directory never
writes its `image.img`.
