//! The compositor's display port: what the display shows and at what scale,
//! read and changed, so one row alone may receive it.

/// The port the compositor serves its display's settings on.
pub const PORT: &str = "display";

/// The one row the build lets receive [`PORT`]: `screen`, an applet of it.
pub const HOLDER: &str = "toybox";

/// The compositor's argument naming the scale its image starts the desktop
/// at where the panel gives none, as `scale=2`: `cargo run --scale` and a
/// test image's row write it.
pub const SCALE_ARG: &str = "scale=";
