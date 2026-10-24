//! Who carries the machine's log off it: the console UART, or the console
//! holder that says another channel does. One word, which the console's one
//! writer reads before each record it would put on the UART.
//!
//! **A holder's end is its release**, so a holder that dies cannot leave the
//! UART quiet; and a fatal path or the stop takes the log back to the UART for
//! good, which nothing undoes.

use core::sync::atomic::{AtomicUsize, Ordering};

const UART: usize = 0;
const FOR_GOOD: usize = 1;

pub struct Carrier(AtomicUsize);

/// Why a holder may not carry the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Another holder carries it.
    Another,
    /// A fatal path or the stop gave it to the UART for good.
    ForGood,
}

impl Carrier {
    pub const fn new() -> Self {
        Self(AtomicUsize::new(UART))
    }

    /// `holder` carries the log from now; answered `Ok` again for the holder
    /// that already does. A holder is an address, which is never 0 or 1.
    pub fn carry(&self, holder: usize) -> Result<(), Refused> {
        assert!(holder > FOR_GOOD, "carrier: {holder:#x} is no holder's address");
        match self.0.compare_exchange(UART, holder, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => Ok(()),
            Err(now) if now == holder => Ok(()),
            Err(FOR_GOOD) => Err(Refused::ForGood),
            Err(_) => Err(Refused::Another),
        }
    }

    /// `holder` no longer carries the log, by its word or by its end: whether
    /// it did.
    pub fn release(&self, holder: usize) -> bool {
        self.0.compare_exchange(holder, UART, Ordering::SeqCst, Ordering::SeqCst).is_ok()
    }

    /// The UART carries everything from here to the machine's end.
    pub fn for_good(&self) {
        self.0.store(FOR_GOOD, Ordering::SeqCst);
    }

    /// Whether a holder carries the log, so the UART takes none of it.
    pub fn elsewhere(&self) -> bool {
        !matches!(self.0.load(Ordering::SeqCst), UART | FOR_GOOD)
    }
}

impl Default for Carrier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOGKEEPER: usize = 0x1000;
    const OTHER: usize = 0x2000;

    #[test]
    fn a_holder_carries_until_it_lets_go() {
        let carrier = Carrier::new();
        assert!(!carrier.elsewhere());
        assert_eq!(carrier.carry(LOGKEEPER), Ok(()));
        assert_eq!(carrier.carry(LOGKEEPER), Ok(()));
        assert!(carrier.elsewhere());
        assert!(carrier.release(LOGKEEPER));
        assert!(!carrier.elsewhere());
        assert!(!carrier.release(LOGKEEPER));
    }

    /// The holder's console goes with its process, which is what a crash of
    /// the holder is: its end releases it, and nothing else need run.
    #[test]
    fn a_holders_end_gives_the_log_back_to_the_uart() {
        let carrier = Carrier::new();
        carrier.carry(LOGKEEPER).expect("the first holder carries");
        // The end of another holder is not this one's.
        assert!(!carrier.release(OTHER));
        assert!(carrier.elsewhere());
        assert!(carrier.release(LOGKEEPER));
        assert!(!carrier.elsewhere());
        assert_eq!(carrier.carry(OTHER), Ok(()), "a later holder may carry it again");
    }

    #[test]
    fn a_second_holder_is_refused() {
        let carrier = Carrier::new();
        carrier.carry(LOGKEEPER).expect("the first holder carries");
        assert_eq!(carrier.carry(OTHER), Err(Refused::Another));
        assert!(carrier.release(LOGKEEPER));
    }

    #[test]
    fn for_good_is_never_undone() {
        let carrier = Carrier::new();
        carrier.carry(LOGKEEPER).expect("the first holder carries");
        carrier.for_good();
        assert!(!carrier.elsewhere());
        assert!(!carrier.release(LOGKEEPER), "the holder no longer carries it to let go");
        assert_eq!(carrier.carry(LOGKEEPER), Err(Refused::ForGood));
        assert!(!carrier.elsewhere());
    }
}
