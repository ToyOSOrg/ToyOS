//! Why a process is asked to quit, as `SYS_PROCESS_QUIT` takes it and a read
//! of the process's notice answers it: one byte, its [`Reason::to_byte`].

/// Why a process is asked to quit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    /// Its user interrupted what it runs, as a terminal's Ctrl+C does.
    Interrupt,
    /// What it runs for is gone, as a closed terminal or a dropped connection is.
    HangUp,
    /// The machine or its parent is ending it.
    Terminate,
}

impl Reason {
    pub const fn to_byte(self) -> u8 {
        match self {
            Reason::Interrupt => 1,
            Reason::HangUp => 2,
            Reason::Terminate => 3,
        }
    }

    /// The reason a byte names, or `None` for a byte [`Self::to_byte`] never makes.
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Reason::Interrupt),
            2 => Some(Reason::HangUp),
            3 => Some(Reason::Terminate),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_reads_back_and_no_other_byte_is_one() {
        for reason in [Reason::Interrupt, Reason::HangUp, Reason::Terminate] {
            assert_eq!(Reason::from_byte(reason.to_byte()), Some(reason));
        }
        let named = (0..=u8::MAX).filter(|&b| Reason::from_byte(b).is_some()).count();
        assert_eq!(named, 3);
    }
}
