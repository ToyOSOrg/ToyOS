//! The question `/system/bin/supervisor` asks the person at the screen before
//! an installed package's launch, and the answer it honours.
//!
//! **Only the supervisor asks, and only its question grants anything.** It
//! asks over [`PORT`], which [`SERVER`] serves and no row receives, so a
//! connection on it is the supervisor's; the answer comes back on that
//! connection and nowhere else, so a program that paints the same question in
//! a window of its own grants nothing. [`SERVER`] draws the question through
//! [`PROMPT`], which it alone receives: the compositor puts a window made
//! there above every other and gives it every key and press while it is up.
//!
//! **One connection per question**: the supervisor's [`Ask`], then the
//! server's [`Reply`]. The supervisor closing it withdraws the question, its
//! caller having hung up; the server closing it is [`Reply::Skip`].

use crate::grants::{Access, Answer, Folder};

/// The port the question is asked on, which no row receives.
pub const PORT: &str = "consent";

/// The one row that serves [`PORT`] and receives [`PROMPT`].
pub const SERVER: &str = "filepicker";

/// The compositor's port for a window in the layer above every other. The row
/// that serves it is the screen's ([`crate::launch::Screen`]).
pub const PROMPT: &str = "prompt";

/// Supervisor → server: an [`Ask`].
pub const MSG_ASK: u32 = 1;
/// Server → supervisor: a [`Reply`].
pub const MSG_REPLY: u32 = 2;

/// The longest [`Ask`]: the longer access, a NUL and the longest name.
pub const MAX_ASK: usize = LONGER_ACCESS + 1 + crate::MAX_PROGRAM_NAME;
/// The longest [`Reply`] whose folder [`crate::grants::folder`] admits: its
/// kind, the longer access, a NUL, and the longest folder, `/` and all. A
/// receiver keeps a byte more, so a longer one is refused rather than read as
/// the prefix kept.
pub const MAX_REPLY: usize = 1 + LONGER_ACCESS + 1 + 1 + crate::grants::MAX_ROOT;
const LONGER_ACCESS: usize = "read-write".len();

/// The question: which package asks for a folder, and what it asks to do there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    pub package: String,
    pub access: Access,
}

impl Ask {
    /// The access, a NUL, then the package's name.
    pub fn encode(&self) -> Vec<u8> {
        format!("{}\0{}", self.access, self.package).into_bytes()
    }

    /// `None` for bytes [`encode`](Self::encode) cannot have written.
    pub fn decode(payload: &[u8]) -> Option<Self> {
        let (access, package) = std::str::from_utf8(payload).ok()?.split_once('\0')?;
        let path = format!("{}/{package}/x", crate::package::DIR);
        let access = Access::parse(access)?;
        (crate::package::package_of(&path) == Some(package)).then(|| Self { package: package.to_string(), access })
    }

    /// The question in plain words, as the prompt says it.
    pub fn words(&self) -> String {
        match self.access {
            Access::ReadWrite => format!("{} asks to open and change the files in one folder you choose.", self.package),
            Access::ReadOnly => format!("{} asks to open the files in one folder you choose.", self.package),
        }
    }
}

/// What the person at the screen answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// This launch holds the folder; nothing is kept.
    Once(Folder),
    /// This launch and every one of the same binary until it is revoked.
    Always(Folder),
    /// No folder, kept until it is revoked.
    Deny,
    /// No folder this launch, and the next launch asks again.
    Skip,
}

const ONCE: u8 = 1;
const ALWAYS: u8 = 2;
const DENY: u8 = 3;
const SKIP: u8 = 4;

impl Reply {
    /// Its kind byte, then, for a folder, the access, a NUL and the path.
    pub fn encode(&self) -> Vec<u8> {
        let (kind, folder) = match self {
            Self::Once(folder) => (ONCE, Some(folder)),
            Self::Always(folder) => (ALWAYS, Some(folder)),
            Self::Deny => (DENY, None),
            Self::Skip => (SKIP, None),
        };
        let mut out = vec![kind];
        if let Some(folder) = folder {
            out.extend_from_slice(format!("{}\0{}", folder.access, folder.path).as_bytes());
        }
        out
    }

    /// `None` for bytes [`encode`](Self::encode) cannot have written. The path
    /// is not judged here: [`judge`] holds it to [`crate::grants::admit`].
    pub fn decode(payload: &[u8]) -> Option<Self> {
        let (&kind, rest) = payload.split_first()?;
        let folder = || {
            let (access, path) = std::str::from_utf8(rest).ok()?.split_once('\0')?;
            Some(Folder { path: path.to_string(), access: Access::parse(access)? })
        };
        match kind {
            ONCE => folder().map(Self::Once),
            ALWAYS => folder().map(Self::Always),
            DENY if rest.is_empty() => Some(Self::Deny),
            SKIP if rest.is_empty() => Some(Self::Skip),
            _ => None,
        }
    }

    /// The folder this launch starts holding.
    pub fn folder(&self) -> Option<&Folder> {
        match self {
            Self::Once(folder) | Self::Always(folder) => Some(folder),
            Self::Deny | Self::Skip => None,
        }
    }

    /// What the store keeps of it: Always's folder and Deny, never Once or Skip.
    pub fn kept(&self) -> Option<Answer> {
        match self {
            Self::Always(folder) => Some(Answer::Granted(folder.clone())),
            Self::Deny => Some(Answer::Denied),
            Self::Once(_) | Self::Skip => None,
        }
    }
}

/// What one end of a parked question said since it was last read.
#[derive(Clone, Copy, Debug)]
pub enum Heard<'a> {
    Nothing,
    /// A whole frame: its type, and its payload as the receiver kept it.
    Frame(u32, &'a [u8]),
    /// It hung up, or sent what no frame is: why, in words.
    Ended(&'static str),
}

/// Where a parked launch goes on what its `caller` and the consent `server`
/// said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Settled {
    /// Neither said anything: it stays parked.
    Waiting,
    /// Its caller hung up: it is not started, whatever was answered.
    Withdrawn,
    /// It starts, holding what the answer gives, or no folder for why it is
    /// none ([`judge`]).
    Answered(Result<Reply, String>),
}

/// A parked launch's next step. **The caller first**: anything on its
/// connection after its launch is its hang-up, and a launch nobody waits for
/// any more is not started, even on an answer already waiting.
pub fn settle(caller: Heard<'_>, server: Heard<'_>, ceiling: Option<Access>) -> Settled {
    if !matches!(caller, Heard::Nothing) {
        return Settled::Withdrawn;
    }
    match server {
        Heard::Nothing => Settled::Waiting,
        Heard::Frame(msg_type, payload) => Settled::Answered(judge(msg_type, payload, ceiling)),
        Heard::Ended(why) => Settled::Answered(Err(why.to_string())),
    }
}

/// The server's frame on a question's connection, as the supervisor honours
/// it: a [`Reply`] whose folder [`crate::grants::admit`] gives under the
/// image's `ceiling`, or why it is none. `payload` is what the receiver kept,
/// a byte past [`MAX_REPLY`] at most, so a longer frame is refused rather than
/// read as the prefix kept. **The server is a program like any**: nothing it
/// says is trusted past this.
pub fn judge(msg_type: u32, payload: &[u8], ceiling: Option<Access>) -> Result<Reply, String> {
    if msg_type != MSG_REPLY {
        return Err(format!("it sent message {msg_type}, which is no answer"));
    }
    if payload.len() > MAX_REPLY {
        return Err(format!("its answer is longer than the {MAX_REPLY} bytes any answer is"));
    }
    let reply = Reply::decode(payload).ok_or("its answer is not one the protocol has")?;
    if let Some(folder) = reply.folder() {
        crate::grants::admit(folder, ceiling)?;
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn games(access: Access) -> Folder {
        Folder { path: "/home/toy/My Games".into(), access }
    }

    #[test]
    fn an_ask_and_a_reply_read_back_as_written_and_nothing_else_reads() {
        for ask in [
            Ask { package: "gbae".into(), access: Access::ReadWrite },
            Ask { package: "n".repeat(crate::MAX_PROGRAM_NAME), access: Access::ReadOnly },
            Ask { package: "n".repeat(crate::MAX_PROGRAM_NAME), access: Access::ReadWrite },
        ] {
            assert!(ask.encode().len() <= MAX_ASK, "{ask:?}");
            assert_eq!(Ask::decode(&ask.encode()), Some(ask));
        }
        let longest = format!("/home/toy/{}", "g".repeat(crate::grants::MAX_ROOT - "home/toy/".len()));
        assert_eq!(crate::grants::folder(&longest), Ok(()));
        for access in [Access::ReadOnly, Access::ReadWrite] {
            let reply = Reply::Always(Folder { path: longest.clone(), access });
            assert!(reply.encode().len() <= MAX_REPLY, "{reply:?}");
        }
        assert_eq!(Reply::Always(Folder { path: longest, access: Access::ReadWrite }).encode().len(), MAX_REPLY);
        for bad in [&b""[..], b"read-write", b"write\0gbae", b"read-write\0", b"read-write\0../x", b"read-write\0a/b", &[0xff, 0]] {
            assert_eq!(Ask::decode(bad), None, "{bad:?}");
        }
        for reply in [
            Reply::Once(games(Access::ReadWrite)),
            Reply::Always(games(Access::ReadOnly)),
            Reply::Deny,
            Reply::Skip,
        ] {
            assert_eq!(Reply::decode(&reply.encode()), Some(reply));
        }
        for bad in [&b""[..], &[0], &[5], &[ONCE], b"\x01read-write", b"\x02write\0/home/toy/Games", b"\x03x", b"\x04\0"] {
            assert_eq!(Reply::decode(bad), None, "{bad:?}");
        }
    }

    /// **Once and Skip are never kept**; Always keeps its folder and Deny is
    /// kept as Deny. A launch holds the folder of Once and Always alone.
    #[test]
    fn only_always_and_deny_are_kept() {
        let folder = games(Access::ReadWrite);
        let table = [
            (Reply::Once(folder.clone()), None, Some(&folder)),
            (Reply::Always(folder.clone()), Some(Answer::Granted(folder.clone())), Some(&folder)),
            (Reply::Deny, Some(Answer::Denied), None),
            (Reply::Skip, None, None),
        ];
        for (reply, kept, holds) in table {
            assert_eq!(reply.kept(), kept, "{reply:?}");
            assert_eq!(reply.folder(), holds, "{reply:?}");
        }
    }

    /// **An answer is honoured only as a grant the image could give**: a
    /// folder inside the home and outside `Apps`, canonical, at most the
    /// ceiling, under a ceiling at all; never a frame longer than any answer,
    /// nor another message. Each refusal is by name.
    #[test]
    fn an_answer_is_judged_as_a_grant_the_image_could_give() {
        let rw = Some(Access::ReadWrite);
        let ro = Some(Access::ReadOnly);
        let judged = |reply: &Reply, ceiling| judge(MSG_REPLY, &reply.encode(), ceiling);
        for (reply, ceiling) in [
            (Reply::Always(games(Access::ReadWrite)), rw),
            (Reply::Once(games(Access::ReadOnly)), rw),
            (Reply::Always(games(Access::ReadOnly)), ro),
            (Reply::Deny, rw),
            (Reply::Skip, None),
        ] {
            assert_eq!(judged(&reply, ceiling), Ok(reply.clone()), "{reply:?} under {ceiling:?}");
        }
        let at = |path: &str| Folder { path: path.into(), access: Access::ReadWrite };
        for (reply, ceiling, said) in [
            (Reply::Always(games(Access::ReadWrite)), ro, "read-write is past this image's read-only"),
            (Reply::Once(games(Access::ReadWrite)), ro, "read-write is past this image's read-only"),
            (Reply::Once(games(Access::ReadOnly)), None, "this image grants no package a folder"),
            (Reply::Always(at("/home/toy/Apps")), rw, "where every app keeps its own folder"),
            (Reply::Once(at("/home/toy/Apps/gbae")), rw, "where every app keeps its own folder"),
            (Reply::Always(at("/home/toy/Games/../Apps")), rw, "is not a canonical path"),
            (Reply::Once(at("/home/toy//Games")), rw, "is not a canonical path"),
            (Reply::Always(at("/state/supervisor")), rw, "is not inside /home/toy"),
            (Reply::Once(at("/home/toy")), rw, "is the home itself"),
        ] {
            match judged(&reply, ceiling) {
                Err(why) if why.contains(said) => {}
                other => panic!("{reply:?} under {ceiling:?}: {other:?}, not {said:?}"),
            }
        }
        // A frame past the longest answer, the receiver keeping a byte more:
        // refused for its length, and not read as the answer it begins with.
        let mut long = Reply::Deny.encode();
        long.resize(MAX_REPLY + 1, 0);
        assert_eq!(judge(MSG_REPLY, &long, rw), Err(format!("its answer is longer than the {MAX_REPLY} bytes any answer is")));
        let mut kept = Reply::Always(games(Access::ReadWrite)).encode();
        kept.resize(MAX_REPLY + 1, b'x');
        assert_eq!(judge(MSG_REPLY, &kept, rw), Err(format!("its answer is longer than the {MAX_REPLY} bytes any answer is")));
        assert_eq!(judge(MSG_ASK, &Reply::Deny.encode(), rw), Err(format!("it sent message {MSG_ASK}, which is no answer")));
        assert_eq!(judge(MSG_REPLY, b"\x03x", rw), Err("its answer is not one the protocol has".to_string()));
    }

    /// **A launch whose caller hung up is withdrawn**, an answer waiting
    /// beside the hang-up included; with the caller still there, the server's
    /// word decides, and silence waits.
    #[test]
    fn a_hang_up_withdraws_the_launch_before_any_answer() {
        let rw = Some(Access::ReadWrite);
        let always = Reply::Always(games(Access::ReadWrite)).encode();
        let answer = Heard::Frame(MSG_REPLY, &always);
        for caller in [Heard::Ended("its caller hung up"), Heard::Frame(9, b"x")] {
            for server in [Heard::Nothing, answer, Heard::Ended("the server ended")] {
                assert_eq!(settle(caller, server, rw), Settled::Withdrawn, "{caller:?}, {server:?}");
            }
        }
        assert_eq!(settle(Heard::Nothing, Heard::Nothing, rw), Settled::Waiting);
        assert_eq!(settle(Heard::Nothing, answer, rw), Settled::Answered(Ok(Reply::Always(games(Access::ReadWrite)))));
        assert_eq!(
            settle(Heard::Nothing, Heard::Ended("the server ended"), rw),
            Settled::Answered(Err("the server ended".to_string()))
        );
    }

    #[test]
    fn the_question_says_what_the_package_may_do_in_plain_words() {
        let ask = |access| Ask { package: "gbae".into(), access }.words();
        assert_eq!(ask(Access::ReadWrite), "gbae asks to open and change the files in one folder you choose.");
        assert_eq!(ask(Access::ReadOnly), "gbae asks to open the files in one folder you choose.");
    }
}
