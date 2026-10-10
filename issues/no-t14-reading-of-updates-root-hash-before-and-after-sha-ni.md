---
status: open
kind: tooling
opened: 2026-10-09
---

# No T14 reading of `update`'s ROOT hash before and after SHA-NI

`update`'s streamed ROOT hash (`userland/update/src/main.rs`, `stream_root`)
moved from `toyos-sha2`'s scalar compression to `toyos-sha2-hw`'s SHA-NI one,
and no machine has timed it on either. `update` reports no hash time, and no
metal row runs `update`.

The loader's ROOT hash, the same function on the same CPU, is not this
reading: it runs on a soft-float UEFI target before ExitBootServices, and
`update` runs in userland under the ToyOS kernel, streaming a ROOT it also
writes.

Exit: the T14's reading of `update`'s ROOT hash on one image, with
`toyos-sha2`'s scalar compression and with `toyos-sha2-hw`'s.
