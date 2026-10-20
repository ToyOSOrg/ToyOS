//! How a process ended, as `SYS_PROCESS_WAIT` answers it: an exit with the
//! code the process chose, a kill, or a fault of one kind. The kinds are the
//! classes POSIX `<signal.h>` gives a hardware fault, so every architecture
//! reports one fault in the same words.

use core::fmt;

/// How a process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum End {
    /// It exited with this code.
    Exited(i32),
    /// It was killed: by a holder of its handle, or by its parent's end.
    Killed,
    /// The kernel ended it for a fault of this kind.
    Fault(Fault),
}

/// A fault's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fault {
    /// An access to memory the process may not reach (`SIGSEGV`).
    Memory,
    /// A misaligned access the CPU refuses (`SIGBUS`).
    Alignment,
    /// An instruction the CPU would not execute (`SIGILL`).
    Instruction,
    /// An erroneous arithmetic operation (`SIGFPE`).
    Arithmetic,
    /// A breakpoint or a trace trap (`SIGTRAP`).
    Trap,
    /// A syscall named a handle the process does not hold, which ends it
    /// (`kernel::object::HandleError`'s rule).
    Handle,
}

const EXITED: u64 = 0;
const KILLED: u64 = 1;
const FAULTS: [Fault; 6] =
    [Fault::Memory, Fault::Alignment, Fault::Instruction, Fault::Arithmetic, Fault::Trap, Fault::Handle];

impl End {
    /// The word `SYS_PROCESS_WAIT` answers: the kind above the low 32 bits and
    /// an exit's code in them, so no end reaches `SyscallError`'s range.
    pub const fn to_word(self) -> u64 {
        match self {
            End::Exited(code) => (EXITED << 32) | code as u32 as u64,
            End::Killed => KILLED << 32,
            End::Fault(fault) => (KILLED + 1 + fault as u64) << 32,
        }
    }

    /// The end a word names, or `None` for a word [`Self::to_word`] never makes.
    pub fn from_word(word: u64) -> Option<Self> {
        let (kind, low) = (word >> 32, word as u32);
        match kind {
            EXITED => Some(End::Exited(low as i32)),
            KILLED if low == 0 => Some(End::Killed),
            _ if low == 0 => kind.checked_sub(KILLED + 1).and_then(|i| FAULTS.get(i as usize)).map(|f| End::Fault(*f)),
            _ => None,
        }
    }

    /// The code an exit chose; `None` for a kill or a fault, which chose none.
    pub const fn code(self) -> Option<i32> {
        match self {
            End::Exited(code) => Some(code),
            End::Killed | End::Fault(_) => None,
        }
    }

    /// An exit with code 0: a kill or a fault never succeeds.
    pub const fn success(self) -> bool {
        matches!(self, End::Exited(0))
    }
}

impl Fault {
    pub const fn name(self) -> &'static str {
        match self {
            Fault::Memory => "memory",
            Fault::Alignment => "alignment",
            Fault::Instruction => "instruction",
            Fault::Arithmetic => "arithmetic",
            Fault::Trap => "trap",
            Fault::Handle => "handle",
        }
    }
}

/// `code=N`, `killed` or `fault=<kind>`: the field the kernel's `exit:`
/// record carries.
impl fmt::Display for End {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            End::Exited(code) => write!(f, "code={code}"),
            End::Killed => f.write_str("killed"),
            End::Fault(fault) => write!(f, "fault={}", fault.name()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syscall::SyscallError;

    fn every_end() -> impl Iterator<Item = End> {
        let exits = [0, 1, 137, 139, -1, i32::MIN, i32::MAX].map(End::Exited);
        exits.into_iter().chain([End::Killed]).chain(FAULTS.map(End::Fault))
    }

    /// Every end crosses the word and back as itself, and no word an end
    /// makes reads as a syscall's refusal.
    #[test]
    fn every_end_crosses_the_word_as_itself() {
        for end in every_end() {
            assert_eq!(End::from_word(end.to_word()), Some(end), "{end:?}");
            assert_eq!(SyscallError::from_u64(end.to_word()), None, "{end:?} reads as a refusal");
        }
    }

    /// An exit of 137 is not a kill and an exit of 139 is not a fault: the
    /// codes a shell spells them by are a program's to choose.
    #[test]
    fn an_exit_is_never_read_as_a_kill_or_a_fault() {
        let words: std::vec::Vec<u64> = every_end().map(End::to_word).collect();
        for (i, a) in words.iter().enumerate() {
            assert!(!words[i + 1..].contains(a), "two ends share the word {a:#x}");
        }
        assert_eq!(End::Exited(137).code(), Some(137));
        assert_eq!(End::Killed.code(), None);
        assert!(!End::Killed.success() && !End::Fault(Fault::Memory).success() && End::Exited(0).success());
    }

    /// A word no end makes names none: a kind past the last fault, or a kill
    /// or fault carrying a code.
    #[test]
    fn a_word_no_end_makes_names_none() {
        let past = (KILLED + 1 + FAULTS.len() as u64) << 32;
        for word in [past, (KILLED << 32) | 1, End::Fault(Fault::Trap).to_word() | 7] {
            assert_eq!(End::from_word(word), None, "{word:#x}");
        }
    }
}
