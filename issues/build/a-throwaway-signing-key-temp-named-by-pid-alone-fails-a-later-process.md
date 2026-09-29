---
status: open
kind: tooling
opened: 2026-09-29
---

# A throwaway signing key's temp file named by pid alone fails a later process

`checkout_throwaway` in `src/signing.rs` mints into
`image-signing-throwaway.<pid>` opened with `create_new`. A process killed
between the open and its `remove_file` leaves that file, and a later process
that gets the same pid fails with `AlreadyExists` instead of minting.

Exit condition: a mint whose temp name cannot collide with a leftover, and a
leftover that no later process trips over.
