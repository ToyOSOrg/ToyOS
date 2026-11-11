//! The shell's quit handling on the host: a `-c` shell's interrupt against
//! the landing of the child it starts, in every order the kernel and the
//! shell's two threads can take.

#[cfg(test)]
#[path = "../../src/quit.rs"]
pub mod quit;

#[cfg(test)]
mod quitting;
