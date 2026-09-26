---
status: open
kind: finding
opened: 2026-09-26
---

# A winit window on ToyOS redraws unpaced

The compositor sends a frame event when a present reaches the panel. Both
winit backends ignore it: turning it into `RedrawRequested` made every app
that draws on redraw redraw forever. So nothing paces redraws either — an app
that asks for the next frame from inside `RedrawRequested` (an animation)
renders as fast as the CPU lets it, where on Wayland winit holds that request
until the compositor's frame callback. `pre_present_notify` is the hook winit
gives a backend for this.

Next review: promote to a defect with a guest measurement of an animating
app's frame rate against the compositor's, or fold into the backends'
headers.
