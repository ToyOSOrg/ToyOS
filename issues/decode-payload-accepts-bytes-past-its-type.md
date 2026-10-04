---
status: open
kind: defect
opened: 2026-09-28
---

# `ipc::decode_payload` accepts bytes past the type it decodes

`toyos::ipc::decode_payload` refuses a payload shorter than its `T` and ignores
whatever follows one. Each caller decides for itself whether trailing bytes are
out of protocol, and the compositor's client frames
(`userland/compositor/src/session.rs`) disagree: `copy_begin` refuses them with
a length check of its own, and `MSG_PRESENT`'s `Rect`, `MSG_SET_CURSOR`'s
style, `ResolutionRequest` and `CreateWindowRequest` accept them.

Owner: `toyos::ipc`.

**Exit**: one trailing-bytes rule in `toyos::ipc` that every `decode_payload`
caller gets, and `copy_begin`'s own check deleted.
