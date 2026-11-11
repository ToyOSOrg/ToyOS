//! A client's two ends of a session page: its requests onto the request ring
//! and the server's completions off the other.
//!
//! **What the server writes ends the session or is a completion the client
//! takes**: a cursor no server writes, words that are no completion, and a
//! completion [`Client::complete`] refuses are each a [`Violation`], at the
//! call that met it. **So does its silence**: [`ClientEnds::patience`] runs
//! out once the client has waited [`SILENCE_BOUND_NS`] for a completion.
//! Either way the caller ends the session, and [`Client::session_ended`]
//! answers what was on the wire.

use toyos::transport::{Run, Violation, Word};

use crate::client::{Client, Placing};
use crate::entry::Completion;
use crate::layout::{self, ClientRings, RING_WORDS};

/// How long a client that is owed a completion goes without one before it
/// takes the server for wedged. Policy: a driver takes back a command its
/// device leaves unanswered and answers it itself (NVMe's after 10 s), so a
/// server silent for several times that is not waiting on a device.
pub const SILENCE_BOUND_NS: u64 = 60_000_000_000;

pub struct ClientEnds {
    rings: ClientRings,
    /// When the client came to be owed a completion, or last took one while
    /// owed more; `None` while it is owed none.
    owed_since: Option<u64>,
}

impl ClientEnds {
    /// The client's ends of `page`, every word it owns set to 0: done before
    /// the page is sent to a server.
    pub fn new<W: Word>(page: &[W; RING_WORDS]) -> Self {
        Self { rings: layout::client(page), owed_since: None }
    }

    /// Put every request `client` will send on the ring, at `now`; `true` if
    /// any went, which the server's doorbell is owed for.
    pub fn send<W: Word, P: Clone>(
        &mut self,
        page: &[W; RING_WORDS],
        client: &mut Client<P>,
        now: u64,
        mut place: impl FnMut(Placing<'_, P>) -> Option<Run>,
    ) -> Result<bool, Violation> {
        while self.rings.0.space(page)? > 0 {
            let Some(request) = client.next_request(&mut place) else { break };
            let pushed = self.rings.0.push(page, request.encode());
            assert_eq!(pushed, Ok(true), "blockring: a request past the room just counted");
        }
        self.owe(client, now, false);
        Ok(self.rings.0.publish(page))
    }

    /// Give `client` every completion the server has posted, at `now`. Those
    /// before a violation are taken.
    pub fn receive<W: Word, P: Clone>(&mut self, page: &[W; RING_WORDS], client: &mut Client<P>, now: u64) -> Result<(), Violation> {
        let mut taken = false;
        let met = loop {
            match self.rings.1.pop(page) {
                Ok(Some(words)) => match Completion::decode(words).ok_or(Violation::Entry).and_then(|c| client.complete(c)) {
                    Ok(()) => taken = true,
                    Err(violation) => break Err(violation),
                },
                Ok(None) => break Ok(()),
                Err(violation) => break Err(violation),
            }
        };
        self.rings.1.release(page);
        self.owe(client, now, taken);
        met
    }

    /// How long from `now` the server still has to post a completion; `None`
    /// while the client is owed none, and `Some(0)` for a server to give up.
    pub fn patience(&self, now: u64) -> Option<u64> {
        self.owed_since.map(|since| SILENCE_BOUND_NS.saturating_sub(now.saturating_sub(since)))
    }

    fn owe<P: Clone>(&mut self, client: &Client<P>, now: u64, taken: bool) {
        self.owed_since = match self.owed_since {
            _ if !client.owed() => None,
            Some(since) if !taken => Some(since),
            _ => Some(now),
        };
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::client::{Ask, Outcome, Ticket};
    use crate::entry::{Request, Status};
    use crate::layout::{ServerRings, ARENA, CQ_TAIL, DEPTH, SQ_HEAD};

    type Page = [AtomicU32; RING_WORDS];

    /// A client, its ends of a page, and the server's ends for a test to
    /// answer through, or to write past.
    struct Session {
        page: Page,
        client: Client<Ticket>,
        ends: ClientEnds,
        server: ServerRings,
    }

    fn place(p: Placing<'_, Ticket>) -> Option<Run> {
        let (ticket, blocks) = match p {
            Placing::Read { ticket, blocks } => (ticket, blocks),
            Placing::Write { payload, blocks } => (*payload, blocks),
        };
        ARENA.run(ticket as u32, blocks)
    }

    impl Session {
        fn open() -> Self {
            let page: Page = core::array::from_fn(|_| AtomicU32::new(0));
            let ends = ClientEnds::new(&page);
            let server = layout::server(&page);
            let mut client = Client::new(1 << 20);
            client.session_started();
            Self { page, client, ends, server }
        }

        fn send(&mut self, now: u64) -> Result<bool, Violation> {
            self.ends.send(&self.page, &mut self.client, now, place)
        }

        fn receive(&mut self, now: u64) -> Result<(), Violation> {
            self.ends.receive(&self.page, &mut self.client, now)
        }

        /// The requests the server finds published.
        fn taken(&mut self) -> Vec<Request> {
            let mut taken = Vec::new();
            while let Some(words) = self.server.0.pop(&self.page).unwrap() {
                taken.push(Request::decode(words, u64::MAX).unwrap());
            }
            self.server.0.release(&self.page);
            taken
        }

        fn post(&mut self, tag: u32, status: Status) {
            assert_eq!(self.server.1.push(&self.page, Completion { tag, status }.encode()), Ok(true));
            self.server.1.publish(&self.page);
        }

        /// What the caller is told once the session is ended.
        fn ended(&mut self) -> Vec<(Ticket, Outcome)> {
            self.client.session_ended();
            self.client.take_answers().collect()
        }
    }

    #[test]
    fn requests_go_out_and_their_completions_come_back() {
        let mut s = Session::open();
        s.client.submit(1, Ask::Read { lba: 0, blocks: 1 });
        s.client.submit(2, Ask::Flush);
        assert_eq!(s.send(0), Ok(true));
        assert_eq!(s.send(0), Ok(false), "nothing new, no doorbell");
        for request in s.taken() {
            s.post(request.tag, Status::Ok);
        }
        assert_eq!(s.receive(0), Ok(()));
        assert_eq!(s.client.take_answers().collect::<Vec<_>>(), [(1, Outcome::Done), (2, Outcome::Durable)]);
        assert_eq!(s.ends.patience(0), None, "owed nothing");
    }

    /// A server whose cursor on the request ring is past what the client
    /// published: the send that meets it says so, and the session's end
    /// answers the request that was out.
    #[test]
    fn a_server_past_the_request_ring_ends_the_session() {
        let mut s = Session::open();
        s.client.submit(1, Ask::Read { lba: 0, blocks: 1 });
        assert_eq!(s.send(0), Ok(true));
        s.page[SQ_HEAD].store(2, Ordering::Release);
        s.client.submit(2, Ask::Read { lba: 1, blocks: 1 });
        assert_eq!(s.send(0), Err(Violation::HeadPastTail));
        assert_eq!(s.ended(), [(1, Outcome::Refused)]);
    }

    #[test]
    fn a_server_past_the_completion_ring_ends_the_session() {
        let mut s = Session::open();
        s.client.submit(1, Ask::Read { lba: 0, blocks: 1 });
        assert_eq!(s.send(0), Ok(true));
        s.page[CQ_TAIL].store(DEPTH + 1, Ordering::Release);
        assert_eq!(s.receive(0), Err(Violation::TailPastDepth));
        assert_eq!(s.ended(), [(1, Outcome::Refused)]);
    }

    /// The first answer is the caller's; the second is refused and answers
    /// nothing, and the request beside it is the session's end's.
    #[test]
    fn a_second_answer_for_one_request_ends_the_session() {
        let mut s = Session::open();
        s.client.submit(1, Ask::Read { lba: 0, blocks: 1 });
        s.client.submit(2, Ask::Read { lba: 1, blocks: 1 });
        assert_eq!(s.send(0), Ok(true));
        let tag = s.taken()[0].tag;
        s.post(tag, Status::Ok);
        s.post(tag, Status::Ok);
        assert_eq!(s.receive(0), Err(Violation::Tag));
        assert_eq!(s.client.take_answers().collect::<Vec<_>>(), [(1, Outcome::Done)]);
        assert_eq!(s.ended(), [(2, Outcome::Refused)]);
    }

    #[test]
    fn an_answer_for_a_request_never_asked_ends_the_session() {
        let mut s = Session::open();
        s.client.submit(1, Ask::Read { lba: 0, blocks: 1 });
        assert_eq!(s.send(0), Ok(true));
        let tag = s.taken()[0].tag;
        s.post(tag + 1, Status::Ok);
        assert_eq!(s.receive(0), Err(Violation::Tag));
        assert_eq!(s.ended(), [(1, Outcome::Refused)]);
    }

    /// Words that are no completion: a status no server writes.
    #[test]
    fn a_completion_that_decodes_to_nothing_ends_the_session() {
        let mut s = Session::open();
        s.client.submit(1, Ask::Read { lba: 0, blocks: 1 });
        assert_eq!(s.send(0), Ok(true));
        let mut words = Completion { tag: s.taken()[0].tag, status: Status::Ok }.encode();
        words[1] = u32::MAX;
        assert_eq!(s.server.1.push(&s.page, words), Ok(true));
        s.server.1.publish(&s.page);
        assert_eq!(s.receive(0), Err(Violation::Entry));
        assert_eq!(s.ended(), [(1, Outcome::Refused)]);
    }

    /// `Lost` is a flush's word. A write answered it is refused with the
    /// write still on the wire, so the session's end answers its caller.
    #[test]
    fn a_write_answered_lost_is_answered_by_the_sessions_end() {
        let mut s = Session::open();
        s.client.submit(1, Ask::Write { lba: 0, blocks: 1, payload: 1 });
        assert_eq!(s.send(0), Ok(true));
        let tag = s.taken()[0].tag;
        s.post(tag, Status::Lost);
        assert_eq!(s.receive(0), Err(Violation::Entry));
        assert_eq!(s.client.take_answers().count(), 0);
        assert_eq!(s.ended(), [(1, Outcome::Refused)]);
        assert_eq!(s.client.take_released().collect::<Vec<_>>(), [ARENA.run(1, 1).unwrap()]);
    }

    /// A server that takes a request and never answers it: its patience runs
    /// from the send, and is gone at the bound.
    #[test]
    fn a_dropped_answer_runs_out_the_servers_patience() {
        let mut s = Session::open();
        assert_eq!(s.ends.patience(5), None);
        s.client.submit(1, Ask::Read { lba: 0, blocks: 1 });
        assert_eq!(s.send(5), Ok(true));
        s.taken();
        assert_eq!(s.ends.patience(5), Some(SILENCE_BOUND_NS));
        assert_eq!(s.receive(9), Ok(()));
        assert_eq!(s.send(9), Ok(false));
        assert_eq!(s.ends.patience(5 + SILENCE_BOUND_NS - 1), Some(1), "looking again buys it nothing");
        assert_eq!(s.ends.patience(5 + SILENCE_BOUND_NS), Some(0));
        assert_eq!(s.ended(), [(1, Outcome::Refused)]);
    }

    /// A completion taken is the server at work: what it still owes is
    /// counted from there.
    #[test]
    fn a_completion_renews_the_servers_patience() {
        let mut s = Session::open();
        s.client.submit(1, Ask::Read { lba: 0, blocks: 1 });
        s.client.submit(2, Ask::Read { lba: 1, blocks: 1 });
        assert_eq!(s.send(0), Ok(true));
        let tag = s.taken()[0].tag;
        s.post(tag, Status::Ok);
        assert_eq!(s.receive(40), Ok(()));
        assert_eq!(s.ends.patience(40), Some(SILENCE_BOUND_NS));
    }

    /// A server that answers and never gives a request entry back: the
    /// requests that cannot go out are owed as those on the wire are.
    #[test]
    fn a_server_that_frees_no_request_entry_is_owed_for_what_cannot_go() {
        let mut s = Session::open();
        for round in 0..2 {
            for ticket in 0..Ticket::from(DEPTH) {
                s.client.submit(ticket, Ask::Read { lba: ticket, blocks: 1 });
            }
            assert_eq!(s.send(7), Ok(round == 0));
            if round == 1 {
                break;
            }
            // Read without a release: the client's entries stay taken.
            let mut tags = Vec::new();
            while let Some(words) = s.server.0.pop(&s.page).unwrap() {
                tags.push(Request::decode(words, u64::MAX).unwrap().tag);
            }
            for tag in tags {
                s.post(tag, Status::Ok);
            }
            assert_eq!(s.receive(7), Ok(()));
            assert_eq!(s.ends.patience(7), None);
        }
        assert_eq!(s.client.on_the_wire().count(), 0);
        assert_eq!(s.ends.patience(7 + SILENCE_BOUND_NS), Some(0));
    }
}
