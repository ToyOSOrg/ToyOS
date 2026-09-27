---
status: open
kind: defect
opened: 2026-09-27
---

# A file server can open every partition blockd serves

init gives each `fsd` role the `block` connector (`receives = ["block"]`), and a
session on it is opened by unique GUID (`toyos_blockring::wire::MSG_OPEN`) for
any partition blockd serves but ROOT. So the log's server can open DATA, and
the boot volume's server — whose volume is read-only — can open a partition to
write. Nothing in the protocol ties a session to the role that asked for it;
what keeps each server on its own partition is the argument init passes it,
which is not authority.

**Exit**: init hands each file server a capability to its own partition's
session and nothing else — a port blockd serves per partition, or a session
init opens and moves — with a negative control: the log's server asking for
DATA is refused.
