---
status: open
kind: defect
opened: 2026-10-09
---

# libc's getsockname and getpeername answer a null buffer with success

`getsockname` and `getpeername` (`userland/libc/src/socket.rs`) answer through
`fill_sockaddr`, which returns having written nothing where the address
buffer or the length pointer is null, and each call then answers 0. A program
that passed a null pointer is told its address was written.

A host refuses it. Measured on Darwin 27.0.0, one C program, each call on a
connected stream:

| | `getsockname` | `getpeername` | ToyOS, read from the code |
|---|---|---|---|
| null buffer, length 16 | -1 `EFAULT` | -1 `EFAULT` | 0, the length left at 16 |
| null length | -1 `EFAULT` | -1 `EFAULT` | 0 |
| null buffer, length 0 | 0, the length written 16 | 0, the length written 16 | 0, the length left at 0 |

Linux is unread; by the review that found this, it reads the buffer only when
the length is not 0, which is Darwin's third row.

`SockaddrIn::answer` (`userland/libc/src/inaddr.rs`), which the host test
`an_address_is_truncated_to_the_callers_buffer_as_the_host_truncates_it`
(`tests/libc-arch/src/internet_addresses.rs`) holds against the host's
`getsockname` at every length, excludes both pointers by its contract, so no
test reaches the difference.

`accept` and `recvfrom` take a null buffer as a caller that wants no address,
which POSIX gives them, and Darwin's `accept` answers a null pair with a
socket; what either answers a buffer with a null length is not read.

**Exit**: the null cases are in the module a host builds, and a test in
`tests/libc-arch` compares the answer, `errno` and the length written back
with the host's own `getsockname` for the table's three rows, on each host
the `host` check runs on; `getpeername` answers through the same function.

**Owner**: libc, `userland/libc`; whoever next changes its address calls.
