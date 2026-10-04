---
status: open
kind: defect
opened: 2026-08-10
---

# A handle that can be moved can always be moved on

Both move paths — `SpawnArgs::endow` and `SYS_HANDLE_SEND` — require
`Rights::TRANSFER` on the handle being moved, and both carry the source's rights
into the destination unchanged. So every handle that arrives by a move arrives
carrying `TRANSFER`, and its new holder can move it on. There is no way to
express "you may map this and nobody else may".

soundd's per-client audio ring would ideally travel as a `shm_h_with_MAP_only`,
so that a client cannot re-send it on. The rights model rules that out: sending
needs `TRANSFER`, so that handle cannot be sent. The same hole is in spawn
endowment.

## What it costs

The pid ACL this replaced was explicitly non-transitive — `shared_memory::grant`
was owner-only, and its doc said why: *"a grantee that could re-grant makes the
owner's ACL transitive, so soundd's per-client audio ring reaches anyone that
client names and the owner is never told."* Under handles that is no longer
true. A client can pass soundd's ring to anybody it can reach.

The practical exposure today is small — a client can memcpy the ring's contents
to a peer anyway, so what leaks is the ability to *write* it — but the property
the design claims is not the property the code has, and that is the part worth
recording.

## What would fix it

A rights word per moved handle, on **both** paths: `EndowEntry` gains one and
`SYS_HANDLE_SEND` takes `[TransferEntry { handle, rights }]` instead of
`[RawHandle]`, with `RIGHTS_UNCHANGED` as the wire encoding
`SYS_HANDLE_DUP` already uses. The endowment design argues against a rights word
on the endow path — "a second place to shrink rights and the first one already
exists" — and that argument is wrong for exactly this case: dup-then-move cannot
express a set without `TRANSFER`, because the move needs it.

Not done because doing it on the send path alone would make the two move
verbs disagree, and doing it on both is an ABI change to a struct already
shipped.

## Revocation is the other answer, and the design already refused it

The shared-memory grant's old argument for having no revocation named its own
trigger: *"with `grant` owner-only, the set that can ever map is exactly the set
the owner named, so revocation has no caller today … It stops being sound the
moment the reachable set is no longer exactly what the owner named — if
delegation or re-grant is reintroduced, or when `SYS_HANDLE_SEND` makes a grant
transferable."* `SYS_HANDLE_SEND` exists, so the trigger has fired: the reachable
set is no longer what the owner named.

That leaves two answers and the rights word above is the cheaper one. **Revocation
proper is rejected by name** in the capability-handle design — shm revoke,
unmap-others and the TLB-shootdown machinery under them are scoped out, forced
reclaim is defined as killing the holder, and the exclusion is to be revisited
only if an arbitration case appears that killing cannot serve. The reason is the
`gpu::set_resolution` hazard: freeing memory a consumer may hold pointers into. A
capability system may refuse to hand out a new mapping; taking one back from a
running process is a different thing. So a `MAP`-without-`TRANSFER` handle is not
merely the cheaper fix, it is the only one on the table.

**Promoted to `defect` 2026-08-25** (finding-lifecycle ruling: an isolation
finding recording an unproven-but-plausible boundary weakness is promoted, not
folded). The rights model cannot spell "you may map this and nobody else may",
so the property the capability design claims is not the property the code has —
soundd's per-client ring is re-sendable by its client. Owed by whoever next
opens the handle ABI: a rights word on `EndowEntry` **and** on
`SYS_HANDLE_SEND`, on one ABI-only PR, since doing either alone makes the two
move verbs disagree.
