---
status: open
kind: defect
opened: 2026-10-02
---

# A ring handed to another process resolves its watches in the receiver's table

An `Inbox` handle carries `DUP` and `TRANSFER` (`ops::initial_rights`), and
`inbox_submit` resolves every handle a watch names in the calling process's
table (`inbox::resolve`): an `OP_WATCH` when it is submitted, and a fired
poll's when its submitter looks at the object again. The ring's page is mapped
into its creator alone, so a process handed the ring runs the submissions the
creator wrote, and looks at the creator's fired polls, against whatever its
own table holds under the creator's handle numbers. It reaches no object it
does not hold.

**Exit**: a ring cannot leave the process that made it, or a watch names its
object by something its registrant's table decided; a test hands a ring to a
child.
