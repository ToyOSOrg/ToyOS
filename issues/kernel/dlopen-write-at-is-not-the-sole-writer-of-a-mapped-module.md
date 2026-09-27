---
status: assigned
kind: finding
opened: 2026-09-27
---

# `LoadedLib::write_at`'s "sole writer" `# Safety` is false in the `dlopen` path (M8)

Held by the orchestrator's ELF-loader track; its next brief carries this.

`LoadedLib::write_at`'s `# Safety` says the caller must be the sole writer of
the module's image for the call's duration. In `load_shared_lib` that holds: the
image is exclusively owned before it is mapped. In `sys_dlopen` it does not.
`map_into` maps the module into the running process first, and only then does
`resolve_dlopen_relocs` / `apply_tpoff_relocs` / `apply_dtpoff_relocs` call
`write_at` — so a peer thread of the same process can be reading (or, for a
`Shared` module's private window, touching) those pages while the relocation
writes land.

For an `Owned` module the writes go to freshly allocated pages the peer has no
handle to yet, so the race is benign in practice; for a `Shared` module the
writes go to the private `rw_alloc`, likewise not yet handed out. But the
`# Safety` contract as written is not the one the `dlopen` caller meets, so it
cannot be the thing that makes those `unsafe` blocks sound.

Exit condition: either the writes move before `map_into`, or the
`# Safety` is restated to the invariant the `dlopen` path actually upholds and
every call site's `SAFETY:` cites it.
