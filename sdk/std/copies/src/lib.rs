//! std's ToyOS modules that name nothing but `core` and std's own `sync` and
//! `ops`, on the host: the turnstile threads sharing one directory's
//! connection are served through in turn.

// What the modules below name as std's own, which a host's std is.
#[cfg(test)]
use std::{ops, sync};

#[cfg(test)]
#[path = "../../sys/turns.rs"]
mod turns;
