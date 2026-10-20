//! The `hda` program's Intel HDA driver, pure: the controller and its codec
//! link ([`controller`]) over `toyos-device-memory`'s boundary, and everything
//! decided from what a codec said about itself — which converter and pins
//! carry sound to a speaker, and the verbs that configure them.
//!
//! No instruction that reaches a device is here: the program implements the
//! boundary over its claim. Pure, the controller is host-tested against a
//! model of one, and the graph traversal against the committed codec dumps of
//! the machines that have to work and against states no machine in reach
//! constructs.
//!
//! Every value a codec supplies is untrusted. Counts are bounded, node ids are
//! checked against the range their function group declared, and a connection
//! list that cycles is a refusal rather than a walk.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod caps;
pub mod config;
pub mod controller;
pub mod graph;
pub mod path;
pub mod probe;
pub mod stream;
pub mod verb;

#[cfg(test)]
mod fixture;

pub use caps::{AmpCaps, ConfigDefault, GainRange, PinCaps, WidgetCaps, WidgetKind};
pub use graph::{Codec, FunctionGroup, Widget};
pub use path::{find_output_path, Hop, OutputPath, PathError, PinSetup};
pub use probe::{enumerate, CodecFault, Verbs};
pub use verb::{Address, Node, Response, Verb};
