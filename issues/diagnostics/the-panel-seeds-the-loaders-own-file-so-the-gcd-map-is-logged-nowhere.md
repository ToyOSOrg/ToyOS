---
status: open
kind: defect
opened: 2026-09-14
---

# The panel seeds the loader's own file, so the GCD map is logged nowhere

`/system/bin/console` seeds the screen from every `*.log` file in `/log`, newest
two, oldest first (`userland/console/src/main.rs:277-286`). `/log` is the root of
the log partition and the bootloader writes `loader.log` there
(`userland/logd/src/store.rs:8-9`, `bootloader/src/loaderlog.rs:51`), so
`loader.log` passes the extension filter. A line the loader writes to its file costs a row of the
kernel's log on the panel exactly as a line it prints to the firmware console
does — the file is not a second channel.

Measured on `bar-placement` at `ffd3f9c2` with the loader's whole GCD map moved
off the console and into `loader.log` alone, by `loaderlog::line` and no
`println!`:

    cargo test screen_console_shell   EXIT=1
    FAIL screen_console_shell: no `i8042:` line above the prompt, and the
    console seeded 22093 bytes of kernel log

and all 34 of the q35 map's lines are in the decoded panel, above a prompt with
no kernel line left over it. The same head with those lines written to neither
channel is `cargo test screen_console_shell` **EXIT=0**, `PASS (2s)`, `30 kernel
log rows above a prompt`.

What that costs today: `bootloader/src/gcd.rs` prints one line per free MMIO
range it hands the kernel and no line for any other descriptor, so the map's
held MMIO descriptors — firmware's *current* allocations, the reading that lets
that file's module header be checked against a real machine — reach no console
and no file. Eighteen of the ThinkPad T14's 75 descriptors are MMIO and five
are logged; eleven of q35's 34 are MMIO and two are logged.

The fix is `/system/bin/console`'s, at `userland/console/src/main.rs:277-286`,
and nobody is holding it. What pays for it is `toyos-metal`'s readback of
`loader.log` (`src/metal.rs:1849`): a machine whose aperture is in neither
channel cannot have that premise checked against it afterwards.

**Exit condition.** `/system/bin/console`'s seed takes only the files
`/system/bin/logd` wrote — `src/bootlog.rs:175-187` already splits a `/log`
listing that way for the host-side reader, so the rule exists and the guest does
not apply it. With that in place `loader.log` costs no panel row, the loader
writes one line per descriptor to its file alone, and `cargo test
screen_console_shell` stays green with the whole map in the readback.
