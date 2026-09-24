---
status: open
kind: defect
opened: 2026-09-23
---

# A two-megabyte ssh upload into a QEMU guest ended in a decryption error

`swap_refusals` (`tests/swapcase`, virtio-net, two CPUs, TCG) sends netd's
2,317,912-byte binary to the guest's sshd over one channel, as one
`channel.data` from the harness's russh client. In one run of four, three
guests at once on the dev host, sshd's session died part-way:

```
sshd: 10.0.2.2:56524: swap ?: refused the channel ended after 2080768 bytes, before the request was whole
sshd: session error: DecryptionError
```

russh's `DecryptionError` is the transport's MAC failing: the bytes sshd read
off its TCP stream were not the bytes the client sent. Nothing in that boot had
swapped anything yet — this is the plain path, host → slirp → virtio-net →
netd's TCP → the connection's pipe → sshd. `sshd_files` moves 1 MiB each way
and has not been seen to fail, so the size, the single write, or the boot's own
record stream flowing through netd at the same moment is what differs.

What is not known: whether a byte is lost, duplicated or reordered, or on which
hop. The swap's own digest would have refused the result had it reached init;
the transport refused it first.

**Owed:** a reproduction that counts — the same upload in a loop against a guest
doing nothing else, then beside the stream — and a byte-exact comparison at
netd's pipe to tell netd's bridge from the stack below it.

Seen again on the e1000e: one `lan_swap` of thirty (`tests/e1000talkcase`), ten
guests at once beside fourteen CPU hogs on the dev host, the same two lines at
the same count, `after 2080768 bytes`. So it is not virtio-net's, and the byte
count repeating across two NICs points above the driver.
