//! toybox's applets that name nothing of ToyOS's SDK, on the host: `cp`'s
//! copy and its refusals, `mv`'s refusal of a rename with nothing copied
//! behind it, `hexdump` against a recorded `xxd`, and the roster `ps` reads.

#[cfg(test)]
#[path = "../../src/cp.rs"]
pub mod cp;
#[cfg(test)]
#[path = "../../src/hexdump.rs"]
pub mod hexdump;
#[cfg(test)]
#[path = "../../src/mv.rs"]
pub mod mv;
#[cfg(test)]
#[path = "../../src/roster.rs"]
pub mod roster;

#[cfg(test)]
mod copying;
#[cfg(test)]
mod dumping;
#[cfg(test)]
mod moving;
#[cfg(test)]
mod roster_reading;
