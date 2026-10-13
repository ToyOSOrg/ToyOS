//! The compositor's clipboard port: every copy made in the session is sent to
//! its one connection, so one row alone may receive it.

/// The port the compositor serves its clipboard's watcher on.
pub const PORT: &str = "clipboard";

/// The one row the build lets receive [`PORT`]: QEMU's clipboard agent.
pub const HOLDER: &str = "vdagent";
