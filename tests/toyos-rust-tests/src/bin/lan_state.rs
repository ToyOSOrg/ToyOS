//! Ask netd what network this machine is on, and exit with the answer.
//!
//! **The exit code is the whole report.** On the ThinkPad T14 there is no
//! serial port and a userland write ends at `Backend::None`, so nothing this
//! program prints reaches the harness; the kernel's `exit: <name> pid=N code=N`
//! record is the one word that crosses, and `toyos_lanstate` is the grammar the
//! host reads it back with.

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
    let header = netd.recv_header().map_err(|_| Refusal::NoNetd)?;
    if header.msg_type != RespType::Result as u32 {
        return Err(Refusal::Unanswered);
    }
    let mut answer = [0u8; ANSWER_LEN];
    let got = netd.recv_bytes(&header, &mut answer).map_err(|_| Refusal::Unanswered)?;
    State::decode(&answer[..got]).ok_or(Refusal::Malformed)
}
