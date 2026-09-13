//! Ask netd what network this machine is on, and exit with the answer, which
//! `toyos_lanstate` is the grammar of.

use toyos::endow;
use toyos::net::RespType;
use toyos_lanstate::{Refusal, State, ANSWER_LEN, ASK};

fn main() {
    std::process::exit(match asked() {
        Ok(state) => state.code(),
        Err(refusal) => refusal.code(),
    });
}

/// One question and one answer, over the port netd already serves.
fn asked() -> Result<State, Refusal> {
    let netd = endow::service("netd").map_err(|_| Refusal::NoNetd)?;
    netd.signal(ASK).map_err(|_| Refusal::NoNetd)?;
    // netd took the question and did not answer it, which is not the same as
    // there being no netd to ask.
    let header = netd.recv_header().map_err(|_| Refusal::Unanswered)?;
    if header.msg_type != RespType::Result as u32 {
        return Err(Refusal::Unanswered);
    }
    // `recv_bytes` truncates a longer frame to the buffer and skips the rest, so
    // bytes this grammar did not write would decode as an answer it did.
    if header.len() as usize != ANSWER_LEN {
        return Err(Refusal::Malformed);
    }
    let mut answer = [0u8; ANSWER_LEN];
    netd.recv_bytes(&header, &mut answer).map_err(|_| Refusal::Unanswered)?;
    Ok(State::decode(&answer))
}
