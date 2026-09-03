---
status: open
kind: defect
opened: 2026-08-03
---

# Tearing is what GOP cannot give back, and two clients still present whole

Recorded from the compositor's rendering pass (task #132), which moved
composition into system RAM and gave `MSG_PRESENT` a damage rect. Three
residuals, none of them fixed there:

**The scanout blit is not synchronised to the panel.** Nothing composes onto
the scanout any more, so a frame is never seen half-composed; what remains is
that the blit of a damage rect can land while the panel is reading those rows,
so that rect can tear. The window shrank from "a whole composite" — a wallpaper
blit, then every window, then the taskbar, then the cursor, each a separate
pass over the mapping — to one `memcpy` of the damaged rows. Closing it needs
to know where the beam is, and GOP does not say: there is no vblank interrupt,
no scanline register and no page flip in the protocol. It is the display
driver's to close, and the owner's ruling is that the driver comes later and
whole. Double-buffering does not help without the flip either — with one
scanout and no way to swap it, a second buffer is what the back buffer already
is.

**Every window client except the terminal presents its whole window.** `paint`,
`files`, `editor`, `filepicker`, and everything on winit/softbuffer (doom,
snake) draw into their shared buffer through a readable `Framebuffer` and keep
no record of where, so `Window::present()`'s "all of it" is the honest answer
for them and `present_damage` is unused. The terminal is the one that composes
through a `window::Screen` and can therefore be asked what it painted. Each of
the others is the same shape of change: compose through `Screen`, hand
`take_damage()` to `present_damage`.

**The back buffer is one screen of RAM charged to nobody.** 8.29 MiB at
1920x1080, on top of the window buffers `issues/isolation/`'s window-cap note already covers.
Same root: there is no per-process memory limit, no pressure signal and no OOM
killer, so a compositor's own working set is bounded by nothing but its code.

## Promoted 2026-08-25

Still reproduces (verified 2026-08-25): `paint`, `files`, `editor` and
`filepicker` still present their whole window — only `userland/terminal` composes
through `window::Screen`. A real, scoped fix is named per client. Owed to
whoever next touches those clients' present path.

## The exit path for residual #2, worked out 2026-08-31

Residual #1 — the scanout blit against the beam — is untouched by any of this
and stays where the owner's ruling left it: GOP publishes no vblank, no scanline
register and no page flip, so the display driver closes it and the driver comes
later and whole. **Residual #2 is the tractable half**, and it is closer than a
reading of this file suggests. Recorded because a wave-4 skip claimed three
things were missing and two of them ship today.

**The frame replay exists.** `Window::present` *is*
`present_damage(Rect { x: 0, y: 0, w: width, h: height })`
(`userland/toyos-window/src/lib.rs:601-603`), and the compositor's `MSG_PRESENT` arm
adds whatever rect the client named with no pixel comparison of its own
(`userland/compositor/src/session.rs:731`). So a client that presents twice with
nothing drawn between makes the compositor recompose and re-blit the same
pixels — which is the "reference capture" half of a pixel-identity check, with
no new mechanism at all.

**A per-client damage oracle exists and is green.** `desktop_typing_damage`
(`tests/toyos.rs:6670`, registered at `:823`, Nightly) reads the compositor's
own `damage_px_max` while a client is driven and refuses a frame over 2% of the
screen. Measured on the dev host 2026-08-31: `eight lines typed, 16
appearances; biggest frame 9472 of 2073600 px over 2 intervals`, PASS. It bounds
damage from *above*, so it catches a converted client that gave up and named
everything.

**The panel does not move once a second.** `tick_taskbar` damages the status
readout alone (`userland/compositor/src/session.rs:1062-1065`), and the comment
on that line says a whole-bar repaint there was the defect that got fixed.

**What is actually missing is a sub-region compare.** `Ppm::identical_to`
(`tests/common/screen.rs:188`) is width, height and every pixel; every other
public method on `Ppm` is whole-image too, and its only callers
(`tests/toyos.rs:3641`, `:4741`) want exactly that. A live-desktop capture pair
differs in the status box whatever the client did — the measurement above is
0.46% of the screen and lands in every interval — so a whole-image compare
answers about the clock and not about the damage rect. `Ppm::pixels` is public,
so the helper is writable rather than blocked; nothing in the tree writes one.

**The claim that helper would settle is the other direction from the gate that
exists.** `present_damage`'s own contract is that a client naming *less* than it
changed leaves a stale screen, so what has to be shown per converted client is
coverage: the damage rect over a sub-region compare of the two captures, equal
everywhere. Neither the size gate nor a whole-image compare shows it.

So the shape is: the sub-region compare on `Ppm`, then one client at a time onto
the terminal's pattern (compose into a `Framebuffer`, blit through a
`window::Screen`, hand `take_damage` to `present_damage` —
`userland/toyos-window/src/framebuffer.rs:85` and `userland/terminal/src/main.rs:36-37`
are the two ends of it), each landing with its own pixel-identity capture pair.
`paint` is 1122 lines, `files` 370, `editor` 1909, `filepicker` 599.
