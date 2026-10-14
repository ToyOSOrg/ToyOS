---
status: open
kind: defect
opened: 2026-10-14
---

# getsockopt answers EOPNOTSUPP where POSIX requires ENOPROTOOPT

`sdk/libc/src/sockopt.rs`'s `kept` refuses a TCP-level option asked of a datagram socket with
`Refusal::NotSupported`, `EOPNOTSUPP`, from `getsockopt`. POSIX.1-2024's `getsockopt` lists
`[ENOPROTOOPT]` "The option is not supported by the protocol" under "shall fail", and XSH 2.3
forbids "a different error number from one required by this volume ... for an error condition
described". `setsockopt` already answers `ENOPROTOOPT`. The rule was Linux's, and
`sdk/libc/arch/src/socket_options.rs` holds it there by differential test against the host.

Owner: `sdk/libc`.

**Exit**: `getsockopt` answers `ENOPROTOOPT` for an option the socket's protocol has none of,
and the differential test names the host's other answer as the host's departure.
