//! Replacing a running service's binary on a machine over its own cable, and
//! what the host heard of it: the ask over ssh, init's words over the record
//! stream, and the machine answering ssh again afterwards.
//!
//! One exchange for a QEMU guest and for the T14: the ask is an `exec` of
//! `/system/bin/swap` through the harness's russh client, and init's
//! verdict is read off the stream in [`toyos_swap::heard`]'s form. **The stream
//! is the one channel that outlives a swap of netd**: the ssh connection that
//! asked goes with the netd that carried it, and this side's connection to
//! `logd` goes with it too, with no FIN and no reset. So a swap of
//! [`toyos_logstream::CARRIER`] is let go only once `logd` has said it turns new
//! readers away until the next netd serves
//! ([`toyos_logstream::CARRIER_LEAVING`]), and this side then dials again
//! ([`Stream::redial`]) — every connection it makes before the old netd is gone
//! turned away, the first one admitted a connection through the new one — so
//! the verdict is whatever init said, as the machine's own log carries it.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::Path;
use std::time::{Duration, Instant};

use toyos_swap::Word;

use crate::metaltalk::{self, Exec, Ssh, Stream, SSH_PORT};

/// How long the host keeps asking for the swap once the stream is open: sshd
/// may still be binding when a boot's stream opens.
const ASK_WINDOW: Duration = Duration::from_secs(30);
const RETRY: Duration = Duration::from_secs(1);

/// How long a refusal's own line has to reach the stream. It is said before
/// the answer is sent, over a stream the refusal left alone.
const REFUSED_WORD: Duration = Duration::from_secs(10);

/// How long `logd`'s [`toyos_logstream::CARRIER_LEAVING`] has to reach this
/// side before the swap goes: `swap` hangs up on init this long after it
/// answered whether or not this side has closed its input, and the old netd is
/// stopped then.
///
/// **Not widened to cover a slow `logd` round, because nothing on this side
/// can.** The line reaches a reader in the round after the one `logd` was in
/// when it was said, and that round's flush may outlast `logd`'s own
/// `LOG_WRITE_BUDGET` on a slow volume; the swap goes on `swap`'s clock whatever
/// this side waits for. A reader that dialled again without the line could be
/// admitted by the netd being stopped and lose its connection with no word, so
/// a missing line is a red verdict rather than a longer wait.
const CARRIER_WORD: Duration = Duration::from_millis(toyos_swap::ANSWER_MS);

/// How many dials a stream's first dial, or a swap's redial, may have turned
/// away — a failed connect, or a connection closed before a line — before it
/// gives up and the boot or the swap is red. Nothing the machine sends says
/// when `logd` listens again after a swap, so a redial asks again at once and
/// this ceiling is the one thing that stops it spinning unseen: the recorded
/// compromise `issues/diagnostics/a-swaps-redial-asks-again-with-no-event-to-wait-on.md`.
pub const TURNED_AWAY_CEILING: usize = 64;

/// What asking for one swap came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Swapped {
    pub service: String,
    /// The digest the host named, which is the binary's own unless the caller
    /// named another.
    pub digest: String,
    /// Who was asked: the peer of the stream when the ask was made.
    pub peer: Ipv4Addr,
    /// The machine's answer, or the client's last refusal to ask it.
    pub answer: Result<String, String>,
    /// Every word init said about this service on the stream after the ask.
    pub words: Vec<(Word, String)>,
    /// The service's own lines on the stream after the ask.
    pub said: Vec<String>,
    /// `echo` asked after init's verdict, through whatever carries the
    /// network then.
    pub again: Result<Exec, String>,
    /// Stream connections before the ask and when this ended.
    pub connections: (usize, usize),
    /// Dials the stream made from the ask on that ended with no line: each
    /// redial `logd` or the machine turned away, refusals included.
    pub turned_away: usize,
    /// From the ask to the answer, to init's final word, and to `echo`'s answer.
    pub answer_ms: u64,
    pub outcome_ms: Option<u64>,
    pub again_ms: u64,
}

impl Swapped {
    /// init's final word, if it said one.
    pub fn outcome(&self) -> Option<&(Word, String)> {
        self.words.iter().rev().find(|(word, _)| word.is_final())
    }
}

/// One swap to ask for.
pub struct Ask<'a> {
    pub service: &'a str,
    pub binary: &'a Path,
    /// The digest sent in place of the binary's own, for a caller whose subject
    /// is the refusal.
    pub named: Option<toyos_swap::Digest>,
}

/// The manifest's name for the program whose lines judge a swap.
const INIT: &str = "init";

/// `line`'s text where it is `tag`'s own line: judged by the head `logd`
/// gives it (`toyos_logstream::program_line`), never by its words, so no other
/// program can say init's word for it.
fn said_by<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    toyos_logstream::program_line(line).filter(|said| said.tag == tag).map(|said| said.text)
}

/// init's word about `service` in `line`, where `line` is init's.
fn init_heard<'a>(line: &'a str, service: &str) -> Option<(Word, &'a str)> {
    said_by(line, INIT).and_then(|text| toyos_swap::heard(text, service))
}

/// Whether the wait for `service`'s outcome is over, reading `lines[mark..]`:
/// init's final word about it. A refusal `swap` made before init heard of the
/// ask is its answer on the channel (`unasked …`), and is never waited for here.
fn settled(lines: &[String], mark: usize, service: &str) -> Option<()> {
    lines[mark.min(lines.len())..]
        .iter()
        .any(|line| init_heard(line, service).is_some_and(|(word, _)| word.is_final()))
        .then_some(())
}

/// Ask the machine that opens `stream` to replace a service's binary, and
/// follow it to init's verdict and to ssh answering again.
///
/// `ssh_at` is where sshd is reached, and `None` is the
/// stream's peer's own port 22; QEMU's forward is the other case.
///
/// `Err` is a boot that never opened the stream within `window`, a binary this
/// host cannot read, or a swap of the stream's own carrier whose `logd` never
/// said it would turn readers away before `swap` let the swap go without
/// this side.
pub fn swap(
    stream: &Stream,
    ssh: &Ssh,
    ssh_at: Option<SocketAddr>,
    ask: &Ask<'_>,
    window: Duration,
    scratch: &Path,
) -> Result<Swapped, String> {
    let (service, binary) = (ask.service, ask.binary);
    let bytes = std::fs::read(binary).map_err(|e| format!("{}: {e}", binary.display()))?;
    let digest = ask.named.unwrap_or_else(|| toyos_swap::digest(&bytes));
    let peer = stream
        .wait_connected(window)
        .ok_or_else(|| format!("no boot opened the record stream within {} s", window.as_secs()))?;
    let SocketAddr::V4(peer) = peer else {
        return Err(format!("the stream's peer is {peer}, which is no IPv4 address"));
    };
    let at = ssh_at.unwrap_or(SocketAddr::V4(SocketAddrV4::new(*peer.ip(), SSH_PORT)));
    let (mark, before, away) = (stream.lines().len(), stream.connections(), stream.turned_away());
    let began = Instant::now();

    let mut answer = Err(String::from("never asked"));
    let mut held = None;
    while began.elapsed() < ASK_WINDOW {
        match ssh.swap(at, service, binary, &digest) {
            Ok(answered) => {
                answer = Ok(answered.said.clone());
                held = Some(answered);
                break;
            }
            Err(why) => {
                println!("  swap: {at} did not take the request yet: {why}");
                answer = Err(why);
                std::thread::sleep(RETRY);
            }
        }
    }
    let answer_ms = began.elapsed().as_millis() as u64;
    println!("  swap: {service} ({} bytes, sha256 {}) answered {answer:?}", bytes.len(), toyos_swap::hex(&digest));

    let heard = |lines: &[String]| -> Vec<(Word, String)> {
        lines[mark.min(lines.len())..]
            .iter()
            .filter_map(|line| init_heard(line, service))
            .map(|(word, detail)| (word, detail.to_string()))
            .collect()
    };
    // A refusal leaves the service as it was, and init's word on it is on the
    // stream at once; an accepted swap is followed to init's final word.
    let accepted = matches!(&answer, Ok(said) if said.starts_with("accepted "));
    let until = match &answer {
        Ok(said) if said.starts_with("refused ") => Some(REFUSED_WORD),
        Ok(said) if said.starts_with("accepted ") || said.starts_with("unanswered ") => Some(window),
        _ => None,
    };
    if accepted && service == toyos_logstream::CARRIER {
        let leaving = stream.wait_until(CARRIER_WORD, |lines| {
            lines[mark.min(lines.len())..]
                .iter()
                .any(|line| said_by(line, toyos_logstream::LOGD) == Some(toyos_logstream::CARRIER_LEAVING))
                .then_some(())
        });
        if leaving.is_none() {
            return Err(format!(
                "`logd` did not say it turns readers away within {} ms of the swap of {service} \
                 being accepted, so `swap` let the swap go without this side, and the connection \
                 it carries may end with no word",
                CARRIER_WORD.as_millis()
            ));
        }
    }
    if let Some(answered) = held {
        if let Err(why) = answered.go() {
            println!("  swap: the client ended on the go: {why}");
        }
    }
    if accepted && service == toyos_logstream::CARRIER {
        stream.redial(window, TURNED_AWAY_CEILING);
    }
    let mut outcome_ms = None;
    if let Some(until) = until {
        if stream.wait_until(until.saturating_sub(began.elapsed()), |lines| settled(lines, mark, service)).is_some() {
            outcome_ms = Some(began.elapsed().as_millis() as u64);
        }
    }
    let again_at = match (ssh_at, stream.peer()) {
        (None, Some(SocketAddr::V4(now))) => SocketAddr::V4(SocketAddrV4::new(*now.ip(), SSH_PORT)),
        _ => at,
    };
    let command = metaltalk::asked();
    let asked_again = Instant::now();
    let mut again = Err(String::from("never asked"));
    while asked_again.elapsed() < ASK_WINDOW {
        again = ssh.exec(again_at, &command, scratch);
        match &again {
            Ok(_) => break,
            Err(why) => {
                println!("  swap: {again_at} did not take `{command}` yet: {why}");
                std::thread::sleep(RETRY);
            }
        }
    }
    let again_ms = began.elapsed().as_millis() as u64;
    let lines = stream.lines();
    Ok(Swapped {
        service: service.to_string(),
        digest: toyos_swap::hex(&digest),
        peer: *peer.ip(),
        answer,
        words: heard(&lines),
        said: lines[mark.min(lines.len())..]
            .iter()
            .filter(|line| said_by(line, service).is_some())
            .cloned()
            .collect(),
        again,
        connections: (before, stream.connections()),
        turned_away: stream.turned_away() - away,
        answer_ms,
        outcome_ms,
        again_ms,
    })
}

/// What a swap was expected to come to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// The new binary is the service.
    InService,
    /// Refused before anything was stopped.
    Refused,
    /// The new binary failed and the one it replaced runs again.
    Restored,
}

/// The findings against `expect`, or what was heard in one line per fact.
///
/// **Every expectation owes the machine answering ssh afterwards**, because
/// whichever binary ended up serving, the service is running only if the path
/// through it works.
pub fn judge(heard: &Swapped, expect: Expect) -> Result<Vec<String>, Vec<String>> {
    let (mut said, mut bad) = (Vec::new(), Vec::new());
    let service = &heard.service;
    let word = |w: Word| heard.words.iter().any(|(word, _)| *word == w);
    let owed_prefix = match expect {
        Expect::Refused => "refused ",
        Expect::InService | Expect::Restored => "accepted ",
    };
    match &heard.answer {
        // A refusal `swap` made itself, before init heard of the ask, is a
        // refusal too, and the one init never speaks about.
        Ok(answer) if answer.starts_with(owed_prefix) || (expect == Expect::Refused && answer.starts_with("unasked ")) => {
            said.push(format!("the machine answered the swap of {service} {answer:?} in {} ms", heard.answer_ms))
        }
        // The answer and the service carrying it can go together: what
        // decides then is init's own word on the stream.
        Ok(answer) if answer.starts_with("unanswered") && expect != Expect::Refused => {
            said.push(format!("the swap of {service} was {answer:?}; init's words decide"))
        }
        other => bad.push(format!("the swap of {service} was answered {other:?}, where `{owed_prefix}…` is owed")),
    }
    match (expect, heard.outcome()) {
        (Expect::InService, Some((Word::InService, detail))) => {
            let path = toyos_swap::installed_path(service, &toyos_swap::parse_hex(&heard.digest).unwrap_or_default());
            if detail.starts_with(&path) {
                said.push(format!("init: {service} is in service from {detail}"));
            } else {
                bad.push(format!("init put {detail:?} in service, where {path} was sent"));
            }
        }
        (Expect::Restored, Some((Word::Restored, detail))) if word(Word::Failed) => {
            said.push(format!("init: the new {service} failed and {detail} is back"))
        }
        (Expect::Refused, _) if !word(Word::Stopping) => said.push(format!(
            "init stopped nothing: {:?}",
            heard.words.iter().map(|(w, d)| format!("{}: {d}", w.as_str())).collect::<Vec<_>>()
        )),
        (_, outcome) => bad.push(format!(
            "init's words on {service} were {:?} ending in {outcome:?}, where {expect:?} is owed \
             (the stream had {} connection(s) before the ask and {} after)",
            heard.words.iter().map(|(w, _)| w.as_str()).collect::<Vec<_>>(),
            heard.connections.0,
            heard.connections.1
        )),
    }
    if heard.turned_away >= TURNED_AWAY_CEILING {
        bad.push(format!(
            "the stream's redial was turned away {} time(s), its ceiling of {TURNED_AWAY_CEILING}, \
             and gave up",
            heard.turned_away
        ));
    } else if heard.service == toyos_logstream::CARRIER {
        said.push(format!(
            "the stream's dials were turned away {} time(s), refusals included, from the ask to \
             the end",
            heard.turned_away
        ));
    }
    match &heard.again {
        Ok(exec) if exec.status == Some(0) && exec.stdout == metaltalk::owed() => said.push(format!(
            "`{}` answered byte for byte afterwards, {} ms after the ask",
            metaltalk::asked(),
            heard.again_ms
        )),
        other => bad.push(format!("`{}` afterwards answered {other:?}", metaltalk::asked())),
    }
    if bad.is_empty() {
        Ok(said)
    } else {
        Err(bad)
    }
}

/// The keys a swap is written under, one `<key> <value>` per line.
const SERVICE: &str = "swap_service";
const DIGEST: &str = "swap_digest";
const PEER: &str = "swap_peer";
const ANSWER: &str = "swap_answer";
const ANSWER_FAILED: &str = "swap_answer_failed";
const WORD: &str = "swap_word";
const SAID: &str = "swap_said";
const AGAIN_STATUS: &str = "swap_again_status";
const AGAIN_STDOUT: &str = "swap_again_stdout";
const AGAIN_FAILED: &str = "swap_again_failed";
const CONNECTIONS: &str = "swap_connections";
const TURNED_AWAY: &str = "swap_turned_away";
const TIMES: &str = "swap_ms";

/// A value on one line, whatever it carried, read back by `unquote`.
fn quote(text: &str) -> String {
    format!("{text:?}")
}

/// [`quote`]'s inverse for what `Debug` renders a `str` as.
fn unquote(text: &str) -> Result<String, String> {
    let inner = text
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .ok_or_else(|| format!("{text:?} is not a quoted value"))?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('u') => {
                let hex: String = chars.by_ref().skip(1).take_while(|c| *c != '}').collect();
                let code = u32::from_str_radix(&hex, 16).map_err(|_| format!("\\u{{{hex}}}"))?;
                out.push(char::from_u32(code).ok_or_else(|| format!("\\u{{{hex}}}"))?);
            }
            other => return Err(format!("an escape {other:?} Debug does not write")),
        }
    }
    Ok(out)
}

impl Swapped {
    pub fn render(&self) -> String {
        let mut out = format!("{SERVICE} {}\n{DIGEST} {}\n{PEER} {}\n", self.service, self.digest, self.peer);
        match &self.answer {
            Ok(answer) => out.push_str(&format!("{ANSWER} {}\n", quote(answer))),
            Err(why) => out.push_str(&format!("{ANSWER_FAILED} {}\n", quote(why))),
        }
        for (word, detail) in &self.words {
            out.push_str(&format!("{WORD} {}\n", quote(&toyos_swap::said(&self.service, *word, detail))));
        }
        for line in &self.said {
            out.push_str(&format!("{SAID} {}\n", quote(line)));
        }
        match &self.again {
            Ok(exec) => {
                match exec.status {
                    Some(code) => out.push_str(&format!("{AGAIN_STATUS} {code}\n")),
                    None => out.push_str(&format!("{AGAIN_STATUS} none\n")),
                }
                out.push_str(&format!("{AGAIN_STDOUT} {}\n", quote(&String::from_utf8_lossy(&exec.stdout))));
            }
            Err(why) => out.push_str(&format!("{AGAIN_FAILED} {}\n", quote(why))),
        }
        out.push_str(&format!("{CONNECTIONS} {} {}\n", self.connections.0, self.connections.1));
        out.push_str(&format!("{TURNED_AWAY} {}\n", self.turned_away));
        let outcome = self.outcome_ms.map_or("none".to_string(), |ms| ms.to_string());
        out.push_str(&format!("{TIMES} {} {outcome} {}\n", self.answer_ms, self.again_ms));
        out
    }

    /// What a readback's swap file says, or `None` where it carries none.
    pub fn parse(text: &str) -> Result<Option<Self>, String> {
        let all = |key: &str| -> Vec<&str> {
            text.lines()
                .filter_map(|line| line.split_once(' ').filter(|(k, _)| *k == key).map(|(_, v)| v))
                .collect()
        };
        let one = |key: &str| -> Option<&str> { all(key).first().copied() };
        let Some(service) = one(SERVICE) else { return Ok(None) };
        let digest = one(DIGEST).ok_or("no swap_digest")?.to_string();
        let peer = one(PEER).ok_or("no swap_peer")?.parse().map_err(|_| "swap_peer is no address")?;
        let answer = match (one(ANSWER), one(ANSWER_FAILED)) {
            (Some(a), None) => Ok(unquote(a)?),
            (None, Some(f)) => Err(unquote(f)?),
            _ => return Err(format!("the swap file's answer keys are not one answer:\n{text}")),
        };
        let mut words = Vec::new();
        for rendered in all(WORD) {
            let line = unquote(rendered)?;
            let (word, detail) = toyos_swap::heard(&line, service)
                .ok_or_else(|| format!("{line:?} is no word of init's on {service}"))?;
            words.push((word, detail.to_string()));
        }
        let said = all(SAID).into_iter().map(unquote).collect::<Result<_, _>>()?;
        let again = match (one(AGAIN_STATUS), one(AGAIN_STDOUT), one(AGAIN_FAILED)) {
            (Some(status), Some(stdout), None) => Ok(Exec {
                status: match status {
                    "none" => None,
                    code => Some(code.parse().map_err(|_| format!("{AGAIN_STATUS} {code}"))?),
                },
                stdout: unquote(stdout)?.into_bytes(),
            }),
            (None, None, Some(f)) => Err(unquote(f)?),
            _ => return Err(format!("the swap file's again keys are not one answer:\n{text}")),
        };
        let numbers = |key: &str| -> Result<Vec<Option<u64>>, String> {
            one(key)
                .ok_or_else(|| format!("no {key}"))?
                .split(' ')
                .map(|n| if n == "none" { Ok(None) } else { n.parse().map(Some).map_err(|_| format!("{key} {n}")) })
                .collect()
        };
        let conns = numbers(CONNECTIONS)?;
        let turned_away = one(TURNED_AWAY)
            .ok_or_else(|| format!("no {TURNED_AWAY}"))?
            .parse()
            .map_err(|_| format!("{TURNED_AWAY} is no count"))?;
        let times = numbers(TIMES)?;
        let (&[Some(before), Some(after)], &[Some(answer_ms), outcome_ms, Some(again_ms)]) =
            (conns.as_slice(), times.as_slice())
        else {
            return Err(format!("the swap file's numbers do not read:\n{text}"));
        };
        Ok(Some(Self {
            service: service.to_string(),
            digest,
            peer,
            answer,
            words,
            said,
            again,
            connections: (before as usize, after as usize),
            turned_away,
            answer_ms,
            outcome_ms,
            again_ms,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `text` as `logd` writes it under `tag`.
    fn line(tag: &str, text: &str) -> String {
        let tag = toyos_logstream::Tag::new(tag).expect("a tag");
        let stamp = "2026-09-24 22:01:37";
        format!("{}\n", toyos_logstream::ProgramLine { stamp, at_ns: 2_940_000_000, tag, text: text.as_bytes() })
    }

    /// init's words and `logd`'s carrier line count only under their own
    /// programs' names: another program printing them settles nothing.
    #[test]
    fn a_judge_reads_the_speaker_from_the_head_and_never_the_words() {
        let words = toyos_swap::said("netd", Word::InService, "/tmp/netd as pid 12");
        assert_eq!(settled(&[line("init", &words)], 0, "netd"), Some(()));
        assert_eq!(settled(&[line("test-runner", &words), line("netd", &words)], 0, "netd"), None);
        assert_eq!(settled(&[line("init", &words)], 1, "netd"), None, "before mark");
        assert_eq!(init_heard(&line("test-runner", &words), "netd"), None);
        let leaving = line("logd", toyos_logstream::CARRIER_LEAVING);
        assert_eq!(said_by(&leaving, toyos_logstream::LOGD), Some(toyos_logstream::CARRIER_LEAVING));
        assert_eq!(said_by(&line("netd", toyos_logstream::CARRIER_LEAVING), toyos_logstream::LOGD), None);
    }

    fn heard(expect: Expect) -> Swapped {
        let digest = toyos_swap::digest(b"netd");
        let path = toyos_swap::installed_path("netd", &digest);
        let (answer, words) = match expect {
            Expect::InService => (
                format!("accepted {path}"),
                vec![
                    (Word::Accepted, format!("{path} replaces /system/bin/netd (pid 7)")),
                    (Word::Stopping, "pid 7 (/system/bin/netd)".into()),
                    (Word::Started, format!("{path} as pid 12; in service if it runs 5000 ms")),
                    (Word::InService, format!("{path} as pid 12")),
                ],
            ),
            Expect::Refused => (
                "refused the binary hashes to x and the request names y".into(),
                vec![(Word::Refused, "the binary hashes to x and the request names y".into())],
            ),
            Expect::Restored => (
                format!("accepted {path}"),
                vec![
                    (Word::Started, format!("{path} as pid 12")),
                    (Word::Failed, format!("{path} ended (exit status: 101) inside 5000 ms")),
                    (Word::Restored, "/system/bin/netd as pid 13".into()),
                ],
            ),
        };
        Swapped {
            service: "netd".into(),
            digest: toyos_swap::hex(&digest),
            peer: Ipv4Addr::new(10, 0, 2, 15),
            answer: Ok(answer),
            words,
            said: vec![line("netd", "netd: DHCP: lease 10.0.2.15/24 from 10.0.2.2, \"x\"")],
            again: Ok(Exec { stdout: metaltalk::owed(), status: Some(0) }),
            connections: (1, 2),
            turned_away: 3,
            answer_ms: 900,
            outcome_ms: Some(7_000),
            again_ms: 9_000,
        }
    }

    /// What the loop writes is what the judge reads, quoted text and all.
    #[test]
    fn a_swap_reads_back_as_it_was_written() {
        for expect in [Expect::InService, Expect::Refused, Expect::Restored] {
            let swapped = heard(expect);
            let back = Swapped::parse(&swapped.render()).expect("it parses").expect("it is one");
            assert_eq!(back, swapped, "{expect:?}");
            judge(&back, expect).unwrap_or_else(|bad| panic!("{expect:?}: {bad:?}"));
        }
        assert_eq!(Swapped::parse("back_secs 3\n"), Ok(None));
    }

    /// Each expectation refuses the others' outcomes, and the machine not
    /// answering afterwards is a finding whatever init said.
    #[test]
    fn the_judge_holds_each_expectation_to_its_own_outcome() {
        for (got, want) in [
            (Expect::InService, Expect::Restored),
            (Expect::Restored, Expect::InService),
            (Expect::InService, Expect::Refused),
            (Expect::Refused, Expect::InService),
        ] {
            assert!(judge(&heard(got), want).is_err(), "{got:?} judged as {want:?}");
        }
        let mut silent = heard(Expect::InService);
        silent.again = Err("connecting: refused".into());
        assert!(judge(&silent, Expect::InService).is_err());
        let mut elsewhere = heard(Expect::InService);
        elsewhere.words.last_mut().unwrap().1 = "/tmp/swap/other/netd as pid 12".into();
        assert!(judge(&elsewhere, Expect::InService).is_err());
        let mut spun = heard(Expect::InService);
        spun.turned_away = TURNED_AWAY_CEILING;
        assert!(judge(&spun, Expect::InService).is_err(), "a redial that reached its ceiling");
    }

    #[test]
    fn unquote_reads_what_debug_writes() {
        for text in ["plain", "a \"quote\"", "tab\tand\nnewline", "back\\slash", "é and \u{1b}"] {
            assert_eq!(unquote(&quote(text)).as_deref(), Ok(text), "{text:?}");
        }
    }
}
