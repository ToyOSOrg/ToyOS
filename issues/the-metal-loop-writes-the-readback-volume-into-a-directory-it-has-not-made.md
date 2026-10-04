---
status: open
kind: tooling
opened: 2026-09-21
---

# The metal loop writes the readback's volume into a directory it has not made

`toyos-metal --readback <dir> --fat32-check` with a `<dir>` that does not exist
flashes, boots the machine, reads the stick — and then exits 2 without a
readback. T14 run 69 ended that way, after a boot that could not be repeated:

```
  run: sudo -n '/usr/bin/dd' 'if=/dev/sda3' 'bs=4M'  (69632 sectors)
toyos-metal: target/metal-delivery/usbbreak-run69/log-partition.img: No such file or directory (os error 2)
EXIT=2
```

`src/metal.rs`'s `run` writes `READBACK_VOLUME` under `--fat32-check` before it
reaches `write_readback`, and `write_readback` is where `create_dir_all` is.
`clear_readback`, which runs before the flash, takes a missing directory for an
empty one. So the one refusal this path can make is made after the machine has
been spent, and the partition it had just read over ssh is dropped with it.

## Exit condition

A `--readback` directory that does not exist is made before the flash, or
refused before it; no run reaches the machine and then fails on a path of the
host's.
