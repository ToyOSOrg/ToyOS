//! What netstack has asked the kernel to watch, so that a pass submits a watch
//! only where its interest changed or the kernel has answered it.
//!
//! **A kernel watch stays armed until it is answered or replaced**, and its
//! answer is what the object holds at the look that answers it
//! (`kernel/src/inbox/polls.rs`). So a watch submitted again with the same
//! handle, interest and token replaces a poll with its twin and costs the
//! submit the poll's registration on every watch it names, for nothing.
//!
//! **The kernel keeps one poll a handle** (`admit` in `kernel/src/inbox/polls.rs`):
//! a watch of a handle under another token withdraws the handle's earlier poll
//! unanswered. So a record is kept by handle, and a watch under another token
//! replaces it as the kernel replaces the poll.
//!
//! **Every poll the kernel keeps is answered, once**: by readiness, by a
//! refusal, or by `NotFound` when a handle it names is closed, so a record here
//! is let go by the answer under its token and never outlives the poll it
//! records. A record whose poll was answered and whose answer is not yet read
//! costs one needless submit after that answer, and never a missed one.

use std::collections::HashMap;

use toyos::poller::Poller;
use toyos::AsHandle;
use toyos_abi::syscall::SyscallError;

/// The watches the kernel holds armed for this process: each handle's token
/// and interest, and each token's handle.
#[derive(Default)]
pub struct Armed {
    by_handle: HashMap<u32, (u64, u32)>,
    by_token: HashMap<u64, u32>,
}

impl Armed {
    /// Whether a watch of `flags` on `handle` under `token` has to be
    /// submitted, recording it as armed where it does.
    pub fn submit(&mut self, token: u64, handle: u32, flags: u32) -> bool {
        let was = self.by_handle.insert(handle, (token, flags));
        if was == Some((token, flags)) {
            return false;
        }
        if let Some((other, _)) = was.filter(|&(other, _)| other != token) {
            self.by_token.remove(&other);
        }
        if let Some(moved) = self.by_token.insert(token, handle).filter(|&moved| moved != handle) {
            self.by_handle.remove(&moved);
        }
        true
    }

    /// The kernel answered the watch under `token`: it is armed no more.
    pub fn answered(&mut self, token: u64) {
        if let Some(handle) = self.by_token.remove(&token) {
            self.by_handle.remove(&handle);
        }
    }
}

/// A [`Poller`] that submits only what [`Armed`] says has to be.
pub struct Watches {
    poller: Poller,
    armed: Armed,
}

impl Watches {
    pub fn new(capacity: u32) -> Self {
        Self { poller: Poller::new(capacity), armed: Armed::default() }
    }

    pub fn watch(&mut self, handle: &impl AsHandle, flags: u32, token: u64) {
        let lent = handle.as_handle();
        if self.armed.submit(token, lent.raw().0, flags) {
            self.poller.watch(&lent, flags, token);
        }
    }

    /// [`Poller::wait_answers`], each answer letting its watch's record go
    /// before `f` sees it.
    pub fn wait_answers(&mut self, min_complete: u32, timeout_nanos: u64, mut f: impl FnMut(u64, Result<u32, SyscallError>)) {
        let armed = &mut self.armed;
        self.poller.wait_answers(min_complete, timeout_nanos, |token, answer| {
            armed.answered(token);
            f(token, answer);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::Armed;

    const TOKEN: u64 = 0x1_0000_0007;
    const READABLE: u32 = 1;
    const WRITABLE: u32 = 2;

    #[test]
    fn an_armed_watch_is_not_submitted_again() {
        let mut armed = Armed::default();
        assert!(armed.submit(TOKEN, 7, READABLE));
        assert!(!armed.submit(TOKEN, 7, READABLE));
        assert!(!armed.submit(TOKEN, 7, READABLE));
    }

    #[test]
    fn an_answered_watch_is_submitted_again() {
        let mut armed = Armed::default();
        assert!(armed.submit(TOKEN, 7, READABLE));
        armed.answered(TOKEN);
        assert!(armed.submit(TOKEN, 7, READABLE));
    }

    #[test]
    fn a_changed_interest_is_submitted() {
        let mut armed = Armed::default();
        assert!(armed.submit(TOKEN, 7, READABLE));
        assert!(armed.submit(TOKEN, 7, READABLE | WRITABLE));
        assert!(armed.submit(TOKEN, 7, READABLE));
        assert!(!armed.submit(TOKEN, 7, READABLE));
    }

    /// A handle number its closed holder freed, named again under the same
    /// token: the closed one's poll ended with it and the new one has none.
    #[test]
    fn a_token_on_another_handle_is_submitted() {
        let mut armed = Armed::default();
        assert!(armed.submit(TOKEN, 7, READABLE));
        assert!(armed.submit(TOKEN, 8, READABLE));
    }

    /// A handle watched under another token is submitted, and so is the first
    /// token's watch of it after that: the kernel withdrew that poll when the
    /// second replaced it.
    #[test]
    fn a_handle_under_another_token_is_submitted_and_so_is_the_first_again() {
        let mut armed = Armed::default();
        assert!(armed.submit(TOKEN, 7, READABLE));
        assert!(armed.submit(TOKEN + 1, 7, READABLE));
        assert!(armed.submit(TOKEN, 7, READABLE));
        assert!(!armed.submit(TOKEN, 7, READABLE));
    }

    /// The first token's answer, read after the handle moved to a second,
    /// lets the second's record stand.
    #[test]
    fn an_answer_under_a_replaced_token_lets_the_handles_record_stand() {
        let mut armed = Armed::default();
        assert!(armed.submit(TOKEN, 7, READABLE));
        assert!(armed.submit(TOKEN + 1, 7, READABLE));
        armed.answered(TOKEN);
        assert!(!armed.submit(TOKEN + 1, 7, READABLE));
    }

    /// The answer of one token lets no other's record go.
    #[test]
    fn an_answer_is_its_own_tokens_alone() {
        let mut armed = Armed::default();
        assert!(armed.submit(TOKEN, 7, READABLE));
        assert!(armed.submit(TOKEN + 1, 8, READABLE));
        armed.answered(TOKEN);
        assert!(!armed.submit(TOKEN + 1, 8, READABLE));
        assert!(armed.submit(TOKEN, 7, READABLE));
    }
}
