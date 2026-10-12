---
status: open
kind: finding
opened: 2026-10-12
---

# The step from netstack's accept to a whole request is bimodal

Found by pull request #883's T14 run, which stamped every lookup netstack
served in two boots of one session: base, without the DNS cache, and cache,
with it ([evidence `a370c0a9`](https://github.com/ToyOSOrg/evidence/tree/a370c0a950b49ca54cea7abbe234a4f597dcdfed/dnscache-metal-804ccd7fc/run)).
The stamps bound the step from `n1`, the return of the wait in which
netstack's pass accepted the client's connection, to `n2`, its read of the
request whole. Read per stamp ([`accread-base.txt`](https://github.com/ToyOSOrg/evidence/blob/aad605669378cdf7ee67c82eccc9eb31070943b2/dnscache-r2/accread-base.txt),
[`accread-cache.txt`](https://github.com/ToyOSOrg/evidence/blob/aad605669378cdf7ee67c82eccc9eb31070943b2/dnscache-r2/accread-cache.txt)),
it has two modes in both arms and nothing between them:

- 0.0285–0.2007 ms on 225 of base's 228 stamps, and 0.0394–0.2220 ms on 249
  of cache's 253;
- 0.3586–0.4056 ms on the other seven: in base, the download's own lookup in
  runs 5 and 6 and `host`'s, and in cache, the download's own lookup in runs
  4, 5 and 6 and `host`'s. `host`'s read 0.362 ms in both.

Hits and misses show it alike, in both arms, so it is no work of the cache,
whose own part of a hit, `n2` to the answer, has a median of 5.3 µs. It is not yet
known to be netstack's: the span also holds the client's write of its request
after its connect returns, and netstack's next pass of its loop up to that
read. The step after the answer, to the client's return, read 0.131–0.209 ms
over the twelve downloads' lookups, the same order as the slow mode's excess.

Both arms ran pull request #861's netstack, whose wait returns only on a
change (`system/netstack/src/watches.rs`): each descends from its head
`443fb5d10`. Both predate #871's kernel, which saves user FP/vector state only
at a switch instead of at every system call, and the span holds system calls
on both sides.

**Exit**: a T14 run whose stamps on both sides of the IPC, the client's write
and its return beside netstack's wake and read, name what the slow mode waits
on; then it is removed, or recorded as a defect in the file that owns that
step, and this file goes.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`, whose
server's loop and stamps the span crosses.
