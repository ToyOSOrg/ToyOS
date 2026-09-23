//! The `toyos-swap` subsystem: a client's replacement for a running service's
//! binary, staged and handed to `/system/bin/init`, which alone can swap it.
//!
//! The channel's input is one [`Header`] line and then exactly its length in
//! bytes. The answer is one line on the channel's stdout — `accepted <path>`
//! with exit status 0, or `refused <why>` with 1 — and it is init's answer
//! except where the request never reached init. **init is told to go by this
//! daemon hanging up on it**, which happens only once the client has closed the
//! channel behind the answer — or [`toyos_swap::ANSWER_MS`] after it, for one
//! that never does: the service being swapped may be the one carrying the
//! answer, so it is not stopped until the answer has arrived.

use std::sync::atomic::{AtomicU64, Ordering};

use toyos::ipc::Connection;
use toyos_swap::{Header, Refusal, Request};

/// What the channel's input has amounted to so far.
pub enum Taken {
    /// The header or the binary is not whole yet.
    More,
    Whole(Header, Vec<u8>),
}

/// Read a request out of what the channel has sent so far.
///
/// **Nothing is acted on before it is whole**, and a channel that sends more
/// than its header promised is refused rather than truncated: the digest is of
/// the bytes the client meant, and a guess at which ones those were is not a
/// binary anyone can vouch for.
pub fn take(buf: &[u8]) -> Result<Taken, Refusal> {
    let Some((header, at)) = Header::take(buf)? else { return Ok(Taken::More) };
    let body = &buf[at..];
    let want = header.len as usize;
    if body.len() > want {
        return Err(Refusal::Malformed(format!(
            "the channel carried {} bytes after a header promising {want}",
            body.len()
        )));
    }
    if body.len() < want {
        return Ok(Taken::More);
    }
    Ok(Taken::Whole(header, body.to_vec()))
}

/// Every stage this daemon makes has a name of its own, so two sessions never
/// write into one file.
static STAGED: AtomicU64 = AtomicU64::new(0);

/// Stage the binary and ask init, answering what init said and the connection
/// the caller holds until its answer to the client is closed.
///
/// Blocking: a file write and one exchange with init, which answers at once.
pub fn ask(header: &Header, body: &[u8]) -> (Result<String, String>, Option<Connection>) {
    let init = match toyos::endow::service(toyos_swap::PORT) {
        Ok(conn) => conn,
        Err(_) => return (Err(Refusal::NoAuthority.to_string()), None),
    };
    let staged = toyos_swap::staged_path(&header.service, STAGED.fetch_add(1, Ordering::Relaxed));
    let written = std::fs::create_dir_all(toyos_swap::STAGING)
        .and_then(|()| std::fs::write(&staged, body));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&staged);
        return (Err(format!("{staged} could not be written: {e}")), None);
    }
    let request = Request { service: header.service.clone(), staged, digest: header.digest };
    if let Err(e) = init.send_bytes(toyos_swap::MSG_SWAP, &request.encode()) {
        let _ = std::fs::remove_file(&request.staged);
        return (Err(format!("init did not take the request: {e:?}")), None);
    }
    let answer = match init.recv_header() {
        Ok(answer) => answer,
        Err(e) => return (Err(format!("init did not answer: {e:?}")), None),
    };
    let mut text = vec![0u8; answer.len() as usize];
    let said = match init.recv_bytes(&answer, &mut text) {
        Ok(n) => String::from_utf8_lossy(&text[..n]).into_owned(),
        Err(e) => return (Err(format!("init's answer did not arrive whole: {e:?}")), None),
    };
    match answer.msg_type {
        toyos_swap::MSG_ACCEPTED => (Ok(said), Some(init)),
        toyos_swap::MSG_REFUSED => (Err(said), None),
        other => (Err(format!("init answered message {other}, which is no swap answer")), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(body: &[u8]) -> Vec<u8> {
        let header =
            Header { service: "netd".into(), digest: toyos_swap::digest(body), len: body.len() as u64 };
        let mut buf = header.render().into_bytes();
        buf.extend_from_slice(body);
        buf
    }

    #[test]
    fn a_request_is_whole_only_at_its_last_byte() {
        let buf = request(b"\x7fELF and the rest");
        for cut in 0..buf.len() {
            assert!(matches!(take(&buf[..cut]), Ok(Taken::More)), "cut at {cut}");
        }
        match take(&buf) {
            Ok(Taken::Whole(header, body)) => {
                assert_eq!(header.service, "netd");
                assert_eq!(body, b"\x7fELF and the rest");
            }
            _ => panic!("the whole request was not taken"),
        }
    }

    #[test]
    fn more_bytes_than_the_header_promised_are_refused() {
        let mut buf = request(b"abc");
        buf.push(b'!');
        assert!(matches!(take(&buf), Err(Refusal::Malformed(_))));
    }
}
