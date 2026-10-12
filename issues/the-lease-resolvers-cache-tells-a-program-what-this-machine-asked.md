---
status: owner
kind: question
opened: 2026-10-12
---

# The lease's resolver's cache tells a program what this machine asked

netstack asks the resolvers the held lease names, and keeps each program's
answers to that program (`toyos-dns`'s views). The resolver upstream keeps one
cache for every client it serves, and netstack cannot partition a cache it
does not hold. A program learns whether anyone asked that resolver for a name
within the name's TTL in two ways:

- it times its own lookup, which comes back sooner when the resolver holds the
  name than when the resolver has to ask the name's servers;
- it sends a query with recursion not desired (RD clear, RFC 1035 §4.1.1) from
  a datagram socket of its own, which the resolver answers from its cache if
  at all.

On a network where this machine is the resolver's only client, that is what
this machine's other programs asked. Only a resolver that iterates from the
root itself, for each view, and asks no shared recursive resolver removes it,
at a round trip per delegation per view.

## The question

Does netstack resolve iteratively for each view, or does ToyOS accept this
channel?

## Exit condition

The owner's answer. If netstack is to close the channel, this file becomes a
`track` whose exit is a test of the node in which one view's lookup of a name
another view's lookup fetched sends no query to the lease's resolver. If the
channel is accepted, this file becomes a `defect` that stays open.
