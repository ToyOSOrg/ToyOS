---
status: open
kind: defect
opened: 2026-09-05
---

# A second launch of a `serves` program panics init, and any launcher client can ask for one

`userland/init/src/main.rs:565` panics when a program's `serves` acceptor has
already been given away:

    panic!("init: `{}` has already been given the `{name}` acceptor", program.name)

Every `serves` program is started once from `[boot] start`, so the acceptor is
gone by the time userland is up. A second launch of that same program therefore
takes this arm — and reaching it needs no authority beyond a `launcher`
connector, which `/system/bin/shell`, every terminal, sshd and the compositor
all hold. **init dying is the machine dying**: it is the only thing that can
create a process, and the one holder of every port nobody has been launched
with yet.

## Reproduction

From a shell on the shipping image:

    /system/bin/compositor

`Command::new` with no endowment and no extra slot asks the launcher;
`resolve` answers `compositor`'s `[programs]` row; `start` reaches the line
above with `acceptors` no longer holding `compositor`. `netd`, `soundd`,
`logd` and `filepicker` are the same shape.

## Why it is a panic today

The comment at the site states the alternative it was rejecting — a program
spawned with a hole where its own service should be — and that is still right.
What is wrong is which side pays: a client's request that init cannot satisfy
is a refused launch (`MSG_REFUSED`), the same as every other thing a client can
ask for and not get, and `serve_launch` already has that arm.

## Exit condition

A launch naming a `serves` program whose acceptor is spent is refused by name,
init survives it, and a guest test drives a second launch of a started daemon
through the launcher and asserts the machine is still up afterwards.
