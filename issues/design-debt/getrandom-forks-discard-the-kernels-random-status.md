---
status: open
kind: defect
opened: 2026-09-26
---

# The getrandom forks discard the kernel's random status

`SYS_RANDOM` refuses when the CPU's DRNG has nothing to give, and the tree's
`toyos_abi::syscall::random` returns that refusal. The three getrandom forks
(`ToyOSOrg/getrandom`, branches `toyos-0.2`, `toyos-0.3`, `toyos-0.4`, in
`forks.toml`) depend on `toyos-abi = "0.1"` from crates.io, whose `random`
returns nothing, and their ToyOS backends return `Ok(())` whatever the kernel
said. A refused draw therefore leaves the caller's buffer as it was and calls
it random. sshd's host key and every session key come from `rand::rng()`,
which is seeded through them.

Exit condition: each fork's ToyOS backend reads the status through a
`toyos-abi` that returns it and maps a refusal to a getrandom `Error`, and the
lockfiles pin those commits.
