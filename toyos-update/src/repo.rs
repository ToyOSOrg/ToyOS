//! The package repository: what a machine may install, as signed statements
//! the bytes then have to match, and every decision the client makes about
//! them.
//!
//! **The format.** A document is printable ASCII in LF-ended lines of words
//! with one space between: a header `toyos-repo <role> <version>`, then
//! `expires <YYYY-MM-DDTHH:MM:SSZ>`, then the role's fields in a fixed order —
//! an unknown, repeated or reordered one is refused — then `sig <key-id>
//! <sshsig>` lines. A signature is an SSHSIG blob in base64 over exactly the
//! bytes before the first `sig` line, in the role's namespace
//! (`toyos-<role>`), so `ssh-keygen -Y sign -n toyos-<role>` makes one and
//! `ssh-keygen -Y verify` checks one. A key ID is the SHA-256 of the key's
//! OpenSSH public blob. Every document has a size cap, read no further.
//!
//! - `root.<N>.txt` declares the keys, and for every [`Role`] which of them
//!   sign it and how many must: **a threshold counts distinct key IDs**, so a
//!   signature repeated is one signature.
//! - `timestamp.txt` names the one current targets by version, length and
//!   SHA-256.
//! - `targets.<M>.txt` names each item by name and target, with a sequence
//!   that never falls, and its archive by a path under the repository, a
//!   length and a SHA-256. The archive is trusted by those two and nothing
//!   else, so its bytes may come from anywhere.
//!
//! **The client** ([`refresh`]) is TUF's workflow over a [`Mirror`]: from the
//! newer of the root the image pins and the one the machine holds, it walks
//! `root.<N+1>.txt` while there is one, each signed by its predecessor's
//! threshold and its own; then the timestamp, then the targets it names, each
//! against the final root's threshold, its expiry, and the floors the image and
//! the machine hold: never a lower version, never an equal one with other
//! bytes, never a lower sequence for an item. A held floor counts only while
//! the final root gives its role the keys the root held beside it did, which is
//! how a rotation recovers from a stolen key's fast-forward. [`Archive`] checks
//! the archive's bytes as they stream.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use toyos_sha2::Sha256;

use crate::sig;
use crate::{sha256, Digest};

/// The largest `root.<N>.txt` read.
pub const ROOT_CAP: usize = 16 << 10;
/// The largest `timestamp.txt` read.
pub const TIMESTAMP_CAP: usize = 4 << 10;
/// The largest `targets.<M>.txt` read, and the most a timestamp may name.
pub const TARGETS_CAP: usize = 1 << 20;
/// The longest archive an item may name.
pub const ARCHIVE_CAP: u64 = 1 << 30;
/// The most root versions one [`refresh`] walks.
pub const ROOT_STEPS: u64 = 32;
/// The longest item name and target.
const NAME_MAX: usize = 64;

const MAGIC: &str = "toyos-repo";

/// The file the current timestamp is at.
pub const TIMESTAMP_FILE: &str = "timestamp.txt";

/// The file root version `version` is at.
pub fn root_file(version: u64) -> String {
    format!("root.{version}.txt")
}

/// The file targets version `version` is at.
pub fn targets_file(version: u64) -> String {
    format!("targets.{version}.txt")
}

/// Who signs a document, and in which namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Root,
    Timestamp,
    Targets,
}

impl Role {
    /// In the order a root names them.
    pub const ALL: [Role; 3] = [Role::Root, Role::Timestamp, Role::Targets];

    pub const fn name(self) -> &'static str {
        match self {
            Role::Root => "root",
            Role::Timestamp => "timestamp",
            Role::Targets => "targets",
        }
    }

    pub const fn namespace(self) -> &'static str {
        match self {
            Role::Root => "toyos-root",
            Role::Timestamp => "toyos-timestamp",
            Role::Targets => "toyos-targets",
        }
    }

    pub const fn cap(self) -> usize {
        match self {
            Role::Root => ROOT_CAP,
            Role::Timestamp => TIMESTAMP_CAP,
            Role::Targets => TARGETS_CAP,
        }
    }
}

/// A document's signed bytes, in its role's namespace.
pub struct Body<'a> {
    pub role: Role,
    pub bytes: &'a [u8],
}

impl sig::Signed for Body<'_> {
    fn namespace(&self) -> &'static str {
        self.role.namespace()
    }
    fn bytes(&self) -> &[u8] {
        self.bytes
    }
}

/// Whose copy a floor is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holder {
    /// The image's, under `/system/etc/pkg`: vouched for by the image's
    /// signature.
    Image,
    /// The machine's, under `/state/pkg`: what this client last accepted.
    Machine,
}

/// Why one signature line does not count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigRefused {
    /// Its key ID is not one the root gives this role.
    NotTheRolesKey,
    /// Not canonical base64 of an Ed25519 SSHSIG blob.
    Encoding,
    /// The blob names a key other than the one its ID does.
    OtherKey,
    /// The blob is in another role's, or another thing's, namespace.
    Namespace,
    /// Not the key's signature over these bytes.
    Signature,
}

impl fmt::Display for SigRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotTheRolesKey => "its key is not one the root gives this role",
            Self::Encoding => "it is not an Ed25519 SSHSIG blob in canonical base64",
            Self::OtherKey => "its blob names another key than its key ID does",
            Self::Namespace => "it is signed in another namespace",
            Self::Signature => "it is not its key's signature over these bytes",
        })
    }
}

/// Why the client installs nothing. Every variant is a refusal by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Past the role's size cap.
    TooLarge { role: Role, cap: usize },
    /// Not this format.
    Malformed { role: Role, line: usize, why: &'static str },
    /// Fewer distinct keys of the role vouch for it than root `root` requires.
    Threshold { role: Role, version: u64, root: u64, valid: usize, needed: usize, first: Option<SigRefused> },
    /// `root.<N>.txt` says another version.
    RootVersion { want: u64, got: u64 },
    /// More than [`ROOT_STEPS`] roots in one walk.
    RootChain,
    /// Past its expiry by the wall clock.
    Expired { role: Role, version: u64, expires: u64, now: u64 },
    /// Below a version the image or the machine holds.
    Rollback { role: Role, version: u64, floor: u64, holder: Holder },
    /// The version a holder holds, with other bytes.
    Changed { role: Role, version: u64, holder: Holder },
    /// A file the workflow requires is not in the repository.
    Absent { file: String },
    /// The mirror could not be read.
    Fetch { file: String, why: String },
    /// The targets is not the length the timestamp names.
    TargetsLength { want: u64, got: u64 },
    /// The targets is not the SHA-256 the timestamp names.
    TargetsDigest { version: u64 },
    /// The targets says another version than the timestamp names.
    TargetsVersion { want: u64, got: u64 },
    /// An item's sequence below one a holder's targets names.
    Sequence { name: String, target: String, sequence: u64, floor: u64, holder: Holder },
    /// An item's sequence equal to a holder's, naming another archive.
    Reissued { name: String, target: String, sequence: u64, holder: Holder },
    /// An archive longer than its item says.
    ArchivePast { length: u64 },
    /// An archive that ended before its item's length.
    ArchiveShort { length: u64, got: u64 },
    /// An archive that is not its item's SHA-256.
    ArchiveDigest,
    /// A copy a holder keeps that is not a document of its role.
    Held { role: Role, holder: Holder },
}

impl fmt::Display for Holder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Holder::Image => "the image",
            Holder::Machine => "this machine",
        })
    }
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { role, cap } => write!(f, "the {} document is past its cap of {cap} bytes", role.name()),
            Self::Malformed { role, line, why } => write!(f, "the {} document's line {line}: {why}", role.name()),
            Self::Threshold { role, version, root, valid, needed, first } => {
                write!(f, "{} {version} carries {valid} valid signature(s) of the {needed} root {root} requires", role.name())?;
                match first {
                    Some(why) => write!(f, "; the first that did not count: {why}"),
                    None => Ok(()),
                }
            }
            Self::RootVersion { want, got } => write!(f, "{} calls itself root {got}", root_file(*want)),
            Self::RootChain => write!(f, "the repository holds more than {ROOT_STEPS} roots past this machine's"),
            Self::Expired { role, version, expires, now } => write!(
                f,
                "{} {version} expired at {} and the clock says {}",
                role.name(),
                time_text(*expires),
                time_text(*now)
            ),
            Self::Rollback { role, version, floor, holder } => {
                write!(f, "{} {version} is below the {floor} {holder} holds", role.name())
            }
            Self::Changed { role, version, holder } => {
                write!(f, "{} {version} is not the {} {version} {holder} holds", role.name(), role.name())
            }
            Self::Absent { file } => write!(f, "the repository has no {file}"),
            Self::Fetch { file, why } => write!(f, "{file}: {why}"),
            Self::TargetsLength { want, got } => {
                write!(f, "the targets is {got} bytes and the timestamp names {want}")
            }
            Self::TargetsDigest { version } => {
                write!(f, "{} is not the SHA-256 the timestamp names", targets_file(*version))
            }
            Self::TargetsVersion { want, got } => {
                write!(f, "{} calls itself targets {got}", targets_file(*want))
            }
            Self::Sequence { name, target, sequence, floor, holder } => {
                write!(f, "{name} for {target} is sequence {sequence}, below the {floor} {holder} holds")
            }
            Self::Reissued { name, target, sequence, holder } => write!(
                f,
                "{name} for {target} is sequence {sequence} with another archive than {holder}'s sequence {sequence}"
            ),
            Self::ArchivePast { length } => write!(f, "the archive runs past its signed {length} bytes"),
            Self::ArchiveShort { length, got } => {
                write!(f, "the archive ended at {got} bytes, short of its signed {length}")
            }
            Self::ArchiveDigest => write!(f, "the archive is not the SHA-256 its item names"),
            Self::Held { role, holder } => write!(f, "the {} {holder} holds is not one", role.name()),
        }
    }
}

/// An Ed25519 public key.
pub type PublicKey = [u8; 32];

const ED25519: &[u8] = b"ssh-ed25519";

/// `string "ssh-ed25519" | string key`: what OpenSSH names a key by.
pub fn public_blob(key: &PublicKey) -> [u8; 51] {
    let mut out = [0u8; 51];
    out[..4].copy_from_slice(&11u32.to_be_bytes());
    out[4..15].copy_from_slice(ED25519);
    out[15..19].copy_from_slice(&32u32.to_be_bytes());
    out[19..].copy_from_slice(key);
    out
}

/// A key's ID: the SHA-256 of its public blob, the digest `ssh-keygen -l`
/// prints in base64.
pub fn key_id(key: &PublicKey) -> Digest {
    sha256(&public_blob(key))
}

/// One role's signers in a root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub threshold: usize,
    /// Ascending, distinct, each declared by the root.
    pub keys: Vec<Digest>,
}

/// `root.<N>.txt`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub version: u64,
    pub expires: u64,
    /// Every key any role names, ascending by ID.
    pub keys: Vec<PublicKey>,
    /// In [`Role::ALL`]'s order.
    pub grants: [Grant; 3],
}

/// `timestamp.txt`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Timestamp {
    pub version: u64,
    pub expires: u64,
    pub targets: Snapshot,
}

/// The targets a timestamp names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub version: u64,
    pub length: u64,
    pub sha256: Digest,
}

/// `targets.<M>.txt`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Targets {
    pub version: u64,
    pub expires: u64,
    /// Ascending by name, then target; no pair twice.
    pub items: Vec<Item>,
}

/// One installable thing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    /// The triple it runs on.
    pub target: String,
    /// Never falls for a name and target: the package's rollback floor.
    pub sequence: u64,
    /// What a person reads; nothing decides by it.
    pub version: String,
    /// The archive's path under the repository.
    pub url: String,
    pub length: u64,
    pub sha256: Digest,
}

impl Root {
    pub fn parse(bytes: &[u8]) -> Result<Self, Refused> {
        Self::of(&document(bytes, Role::Root)?)
    }

    pub fn grant(&self, role: Role) -> &Grant {
        &self.grants[role as usize]
    }

    fn key(&self, id: &Digest) -> Option<&PublicKey> {
        self.keys.iter().find(|k| key_id(k) == *id)
    }

    fn of(doc: &Doc<'_>) -> Result<Self, Refused> {
        let mut c = doc.fields();
        let mut keys: Vec<PublicKey> = Vec::new();
        let mut last: Option<Digest> = None;
        while c.is("key") {
            let words: Vec<&str> = c.take("key")?.split(' ').collect();
            let [id, kind, blob] = words[..] else {
                return Err(c.last("a key is not `key <id> ssh-ed25519 <public key>`"));
            };
            let id = hex32(id).ok_or_else(|| c.last("a key ID that is not 64 lowercase hex digits"))?;
            let blob = base64_decode(blob).filter(|_| kind == "ssh-ed25519");
            let key: PublicKey = blob
                .filter(|b| b.len() == 51 && b[..19] == public_blob(&[0; 32])[..19])
                .map(|b| b[19..].try_into().expect("32 bytes"))
                .ok_or_else(|| c.last("a key that is not an ssh-ed25519 public key"))?;
            if key_id(&key) != id {
                return Err(c.last("a key ID that is not its key's SHA-256"));
            }
            if last.is_some_and(|l| l >= id) {
                return Err(c.last("keys out of order or repeated"));
            }
            last = Some(id);
            keys.push(key);
        }
        if keys.is_empty() {
            return Err(c.here("a root that declares no key"));
        }
        let ids: Vec<Digest> = keys.iter().map(key_id).collect();
        let mut grants = Vec::new();
        for role in Role::ALL {
            let words: Vec<&str> = c.take("role")?.split(' ').collect();
            if words.len() < 3 || words[0] != role.name() {
                return Err(c.last("roles are not `role <name> <threshold> <key-id>…` for root, timestamp, targets"));
            }
            let named = words.len() - 2;
            let threshold = number(words[1])
                .filter(|t| (1..=named as u64).contains(t))
                .ok_or_else(|| c.last("a threshold that is not between 1 and the keys its role names"))?;
            let mut keys: Vec<Digest> = Vec::new();
            for word in &words[2..] {
                let id = hex32(word).ok_or_else(|| c.last("a key ID that is not 64 lowercase hex digits"))?;
                if !ids.contains(&id) {
                    return Err(c.last("a role names a key the root does not declare"));
                }
                if keys.last().is_some_and(|l| *l >= id) {
                    return Err(c.last("a role's keys out of order or repeated"));
                }
                keys.push(id);
            }
            grants.push(Grant { threshold: threshold as usize, keys });
        }
        c.end()?;
        let grants: [Grant; 3] = grants.try_into().expect("one grant per role");
        Ok(Root { version: doc.version, expires: doc.expires, keys, grants })
    }
}

impl Timestamp {
    pub fn parse(bytes: &[u8]) -> Result<Self, Refused> {
        Self::of(&document(bytes, Role::Timestamp)?)
    }

    fn of(doc: &Doc<'_>) -> Result<Self, Refused> {
        let mut c = doc.fields();
        let words: Vec<&str> = c.take("targets")?.split(' ').collect();
        let targets = snapshot(&words)
            .ok_or_else(|| c.last("`targets` is not `<version> <length> <sha256>` within the targets cap"))?;
        c.end()?;
        Ok(Timestamp { version: doc.version, expires: doc.expires, targets })
    }
}

fn snapshot(words: &[&str]) -> Option<Snapshot> {
    let [version, length, digest] = words else { return None };
    Some(Snapshot {
        version: number(version).filter(|v| (1..u64::MAX).contains(v))?,
        length: number(length).filter(|l| (1..=TARGETS_CAP as u64).contains(l))?,
        sha256: hex32(digest)?,
    })
}

impl Targets {
    pub fn parse(bytes: &[u8]) -> Result<Self, Refused> {
        Self::of(&document(bytes, Role::Targets)?)
    }

    fn find(&self, name: &str, target: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.name == name && i.target == target)
    }

    fn of(doc: &Doc<'_>) -> Result<Self, Refused> {
        let mut c = doc.fields();
        let mut items: Vec<Item> = Vec::new();
        while c.is("item") {
            let name = c.word("item")?;
            if !is_name(name) {
                return Err(c.last("an item name that is not 1 to 64 of a-z, 0-9, `.`, `_`, `-`, first not `.`"));
            }
            let target = c.word("target")?;
            if !is_name(target) {
                return Err(c.last("a target that is not 1 to 64 of a-z, 0-9, `.`, `_`, `-`, first not `.`"));
            }
            let sequence = number(c.word("sequence")?).ok_or_else(|| c.last("a sequence that is not a number"))?;
            let version = c.word("version")?;
            if version.len() > NAME_MAX || version.contains('"') {
                return Err(c.last("a version past 64 bytes or with a quote in it"));
            }
            let url = c.word("url")?;
            if !is_relative(url) {
                return Err(c.last("a url that is not a path under the repository"));
            }
            let length = number(c.word("length")?)
                .filter(|l| (1..=ARCHIVE_CAP).contains(l))
                .ok_or_else(|| c.last("a length that is not between 1 and the archive cap"))?;
            let sha256 = hex32(c.word("sha256")?).ok_or_else(|| c.last("a sha256 that is not 64 lowercase hex digits"))?;
            if items.last().is_some_and(|l| (l.name.as_str(), l.target.as_str()) >= (name, target)) {
                return Err(c.last("items out of order, or one name and target twice"));
            }
            items.push(Item {
                name: name.into(),
                target: target.into(),
                sequence,
                version: version.into(),
                url: url.into(),
                length,
                sha256,
            });
        }
        c.end()?;
        Ok(Targets { version: doc.version, expires: doc.expires, items })
    }
}

/// A repository, as the client reads one: a local directory in this stage.
pub trait Mirror {
    /// The file `name` under the repository, read to no more than `cap + 1`
    /// bytes, so a file past `cap` is seen to be without being read; `None`
    /// where the repository has no such file.
    fn fetch(&mut self, name: &str, cap: usize) -> Result<Option<Vec<u8>>, String>;
}

/// The documents one holder keeps: a root always, the others once accepted.
#[derive(Clone, Copy)]
pub struct Held<'a> {
    pub root: &'a [u8],
    pub timestamp: Option<&'a [u8]>,
    pub targets: Option<&'a [u8]>,
}

impl<'a> Held<'a> {
    fn of(&self, role: Role) -> Option<&'a [u8]> {
        match role {
            Role::Root => Some(self.root),
            Role::Timestamp => self.timestamp,
            Role::Targets => self.targets,
        }
    }
}

/// What a [`refresh`] accepted: the three documents' bytes, for the machine to
/// hold, and the targets.
pub struct Fresh {
    pub root: Vec<u8>,
    pub timestamp: Vec<u8>,
    pub targets_bytes: Vec<u8>,
    pub targets: Targets,
}

/// The repository's current targets, or the refusal naming why not.
///
/// `image` is what the image pins, `machine` what this machine accepted last;
/// `now` is the wall clock in Unix seconds.
pub fn refresh(mirror: &mut dyn Mirror, image: Held<'_>, machine: Option<Held<'_>>, now: u64) -> Result<Fresh, Refused> {
    let held_root = |held: Held<'_>, holder| Root::parse(held.root).map_err(|_| Refused::Held { role: Role::Root, holder });
    let image_root = held_root(image, Holder::Image)?;
    let machine_root = machine.map(|m| held_root(m, Holder::Machine)).transpose()?;
    let holders = [(Holder::Image, Some(image), Some(&image_root)), (Holder::Machine, machine, machine_root.as_ref())];

    let (mut root, mut root_bytes) = match (&machine_root, machine) {
        (Some(m), Some(held)) if m.version > image_root.version => (m.clone(), held.root.to_vec()),
        (Some(m), Some(held)) if m.version == image_root.version && held.root != image.root => {
            return Err(Refused::Changed { role: Role::Root, version: m.version, holder: Holder::Image });
        }
        _ => (image_root.clone(), image.root.to_vec()),
    };
    let mut steps = 0;
    loop {
        let want = root.version + 1;
        let Some(bytes) = fetch(mirror, &root_file(want), ROOT_CAP)? else { break };
        if steps == ROOT_STEPS {
            return Err(Refused::RootChain);
        }
        steps += 1;
        let doc = document(&bytes, Role::Root)?;
        let next = Root::of(&doc)?;
        if next.version != want {
            return Err(Refused::RootVersion { want, got: next.version });
        }
        vouched(&doc, &root)?;
        vouched(&doc, &next)?;
        root = next;
        root_bytes = bytes;
    }
    unexpired(Role::Root, root.version, root.expires, now)?;

    // Each holder's copy of a role counts while the final root gives the role
    // the keys the root held beside it did.
    let floors = |role: Role| -> Vec<(Holder, &[u8])> {
        let mut out = Vec::new();
        for (holder, held, held_root) in holders {
            let (Some(held), Some(held_root)) = (held, held_root) else { continue };
            let Some(bytes) = held.of(role) else { continue };
            if held_root.grant(role).keys == root.grant(role).keys {
                out.push((holder, bytes));
            }
        }
        out
    };

    let timestamp_floors = floors(Role::Timestamp);
    let timestamp_bytes =
        fetch(mirror, TIMESTAMP_FILE, TIMESTAMP_CAP)?.ok_or_else(|| Refused::Absent { file: TIMESTAMP_FILE.into() })?;
    let doc = document(&timestamp_bytes, Role::Timestamp)?;
    let timestamp = Timestamp::of(&doc)?;
    vouched(&doc, &root)?;
    for &(holder, held) in &timestamp_floors {
        let floor = document(held, Role::Timestamp).map_err(|_| Refused::Held { role: Role::Timestamp, holder })?;
        not_back(Role::Timestamp, timestamp.version, &timestamp_bytes, floor.version, held, holder)?;
    }
    unexpired(Role::Timestamp, timestamp.version, timestamp.expires, now)?;

    let snapshot = timestamp.targets;
    let mut targets_floors = Vec::new();
    for (holder, held) in floors(Role::Targets) {
        let floor = Targets::parse(held).map_err(|_| Refused::Held { role: Role::Targets, holder })?;
        if snapshot.version < floor.version {
            return Err(Refused::Rollback { role: Role::Targets, version: snapshot.version, floor: floor.version, holder });
        }
        targets_floors.push((holder, held, floor));
    }
    let file = targets_file(snapshot.version);
    let targets_bytes = fetch(mirror, &file, snapshot.length as usize)?.ok_or(Refused::Absent { file })?;
    if targets_bytes.len() as u64 != snapshot.length {
        return Err(Refused::TargetsLength { want: snapshot.length, got: targets_bytes.len() as u64 });
    }
    if sha256(&targets_bytes) != snapshot.sha256 {
        return Err(Refused::TargetsDigest { version: snapshot.version });
    }
    let doc = document(&targets_bytes, Role::Targets)?;
    let targets = Targets::of(&doc)?;
    if targets.version != snapshot.version {
        return Err(Refused::TargetsVersion { want: snapshot.version, got: targets.version });
    }
    vouched(&doc, &root)?;
    for (holder, held, floor) in &targets_floors {
        not_back(Role::Targets, targets.version, &targets_bytes, floor.version, held, *holder)?;
        for item in &targets.items {
            let Some(was) = floor.find(&item.name, &item.target) else { continue };
            if item.sequence < was.sequence {
                return Err(Refused::Sequence {
                    name: item.name.clone(),
                    target: item.target.clone(),
                    sequence: item.sequence,
                    floor: was.sequence,
                    holder: *holder,
                });
            }
            if item.sequence == was.sequence && (item.length, item.sha256) != (was.length, was.sha256) {
                return Err(Refused::Reissued {
                    name: item.name.clone(),
                    target: item.target.clone(),
                    sequence: item.sequence,
                    holder: *holder,
                });
            }
        }
    }
    unexpired(Role::Targets, targets.version, targets.expires, now)?;

    Ok(Fresh { root: root_bytes, timestamp: timestamp_bytes, targets_bytes, targets })
}

/// An item's archive, checked as it streams: never past the signed length,
/// and the signed SHA-256 at its end.
pub struct Archive {
    length: u64,
    sha256: Digest,
    seen: u64,
    hash: Sha256,
}

impl Archive {
    pub fn of(item: &Item) -> Self {
        Archive { length: item.length, sha256: item.sha256, seen: 0, hash: Sha256::new() }
    }

    /// The next bytes, refused where they run past the signed length.
    pub fn take(&mut self, chunk: &[u8]) -> Result<(), Refused> {
        let seen = self.seen.saturating_add(chunk.len() as u64);
        if seen > self.length {
            return Err(Refused::ArchivePast { length: self.length });
        }
        self.hash.update(chunk);
        self.seen = seen;
        Ok(())
    }

    /// Whether what streamed is the whole archive the item names.
    pub fn finish(self) -> Result<(), Refused> {
        if self.seen < self.length {
            return Err(Refused::ArchiveShort { length: self.length, got: self.seen });
        }
        if self.hash.finalize() != self.sha256 {
            return Err(Refused::ArchiveDigest);
        }
        Ok(())
    }
}

fn fetch(mirror: &mut dyn Mirror, file: &str, cap: usize) -> Result<Option<Vec<u8>>, Refused> {
    mirror.fetch(file, cap).map_err(|why| Refused::Fetch { file: file.into(), why })
}

fn unexpired(role: Role, version: u64, expires: u64, now: u64) -> Result<(), Refused> {
    if now >= expires {
        return Err(Refused::Expired { role, version, expires, now });
    }
    Ok(())
}

/// Never below a held version, and an equal one is the held bytes.
fn not_back(role: Role, version: u64, bytes: &[u8], floor: u64, held: &[u8], holder: Holder) -> Result<(), Refused> {
    if version < floor {
        return Err(Refused::Rollback { role, version, floor, holder });
    }
    if version == floor && bytes != held {
        return Err(Refused::Changed { role, version, holder });
    }
    Ok(())
}

/// Whether `by`'s threshold of distinct keys for the document's role signed
/// it.
fn vouched(doc: &Doc<'_>, by: &Root) -> Result<(), Refused> {
    let grant = by.grant(doc.role);
    let body = Body { role: doc.role, bytes: doc.signed };
    let mut counted: Vec<Digest> = Vec::new();
    let mut first = None;
    for (id, line) in &doc.sigs {
        let public = by.key(id).filter(|_| grant.keys.contains(id));
        let verdict = match public {
            None => Err(SigRefused::NotTheRolesKey),
            Some(public) => check(line, public, &body),
        };
        match verdict {
            Ok(()) if !counted.contains(id) => counted.push(*id),
            Ok(()) => {}
            Err(why) => {
                first.get_or_insert(why);
            }
        }
    }
    if counted.len() < grant.threshold {
        return Err(Refused::Threshold {
            role: doc.role,
            version: doc.version,
            root: by.version,
            valid: counted.len(),
            needed: grant.threshold,
            first,
        });
    }
    Ok(())
}

/// One `sig` line's blob, against the key its ID names.
fn check(blob: &str, public: &PublicKey, body: &Body<'_>) -> Result<(), SigRefused> {
    let blob = base64_decode(blob).ok_or(SigRefused::Encoding)?;
    let (key, namespace, signature) = sshsig(&blob).ok_or(SigRefused::Encoding)?;
    if key != public_blob(public) {
        return Err(SigRefused::OtherKey);
    }
    if namespace != body.role.namespace().as_bytes() {
        return Err(SigRefused::Namespace);
    }
    sig::verify(public, body, &signature).map_err(|_| SigRefused::Signature)
}

/// `(public key blob, namespace, signature)` out of an SSHSIG blob held to
/// `PROTOCOL.sshsig` for Ed25519, or `None`.
fn sshsig(blob: &[u8]) -> Option<(&[u8], &[u8], [u8; 64])> {
    let mut rest = blob.strip_prefix(b"SSHSIG")?;
    if take(&mut rest, 4)? != 1u32.to_be_bytes() {
        return None;
    }
    let key = string(&mut rest)?;
    let namespace = string(&mut rest)?;
    let reserved = string(&mut rest)?;
    let hash = string(&mut rest)?;
    let mut signature = string(&mut rest)?;
    if !rest.is_empty() || !reserved.is_empty() || hash != b"sha512" || string(&mut signature)? != ED25519 {
        return None;
    }
    let raw: [u8; 64] = string(&mut signature)?.try_into().ok()?;
    signature.is_empty().then_some((key, namespace, raw))
}

fn take<'a>(bytes: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    let (head, rest) = bytes.split_at_checked(n)?;
    *bytes = rest;
    Some(head)
}

fn string<'a>(bytes: &mut &'a [u8]) -> Option<&'a [u8]> {
    let len = u32::from_be_bytes(take(bytes, 4)?.try_into().expect("four bytes"));
    take(bytes, len as usize)
}

/// A document split into its header, fields and signatures.
struct Doc<'a> {
    role: Role,
    version: u64,
    expires: u64,
    /// The bytes the signatures are over: everything before the first `sig`.
    signed: &'a [u8],
    lines: Vec<&'a str>,
    /// The lines between `expires` and the first `sig`.
    fields: core::ops::Range<usize>,
    sigs: Vec<(Digest, &'a str)>,
}

fn document(bytes: &[u8], role: Role) -> Result<Doc<'_>, Refused> {
    if bytes.len() > role.cap() {
        return Err(Refused::TooLarge { role, cap: role.cap() });
    }
    let bad = |line: usize, why| Refused::Malformed { role, line, why };
    if let Some(at) = bytes.iter().position(|&b| b != b'\n' && !(b' '..=b'~').contains(&b)) {
        let line = bytes[..at].iter().filter(|&&b| b == b'\n').count() + 1;
        return Err(bad(line, "a byte that is not printable ASCII"));
    }
    let text = core::str::from_utf8(bytes).expect("printable ASCII is UTF-8");
    let Some(text) = text.strip_suffix('\n') else {
        return Err(bad(text.split('\n').count(), "the last line has no newline"));
    };
    let lines: Vec<&str> = text.split('\n').collect();
    if let Some(at) =
        lines.iter().position(|l| l.is_empty() || l.starts_with(' ') || l.ends_with(' ') || l.contains("  "))
    {
        return Err(bad(at + 1, "a line that is not words with one space between"));
    }
    let first_sig = lines.iter().position(|l| l.starts_with("sig ")).unwrap_or(lines.len());
    let signed = &bytes[..lines[..first_sig].iter().map(|l| l.len() + 1).sum::<usize>()];
    let version = match lines[0].split(' ').collect::<Vec<_>>()[..] {
        // Below the largest, so a version always has a next one.
        [MAGIC, name, version] if name == role.name() => number(version).filter(|v| (1..u64::MAX).contains(v)),
        _ => None,
    }
    .ok_or_else(|| bad(1, "the header is not `toyos-repo <role> <version>` for this role, 1 to 2^64 - 2"))?;
    let mut sigs = Vec::new();
    for (at, line) in lines.iter().enumerate().skip(first_sig) {
        let sig = match line.split(' ').collect::<Vec<_>>()[..] {
            ["sig", id, blob] => hex32(id).map(|id| (id, blob)),
            _ => None,
        };
        sigs.push(sig.ok_or_else(|| bad(at + 1, "a line among the signatures that is not `sig <key-id> <sshsig>`"))?);
    }
    let mut doc = Doc { role, version, expires: 0, signed, lines, fields: 1..first_sig, sigs };
    let mut c = doc.fields();
    let expires = c.word("expires")?;
    doc.expires = parse_time(expires).ok_or_else(|| c.last("`expires` is not a YYYY-MM-DDTHH:MM:SSZ that exists"))?;
    doc.fields.start = 2;
    Ok(doc)
}

impl<'a> Doc<'a> {
    fn fields(&self) -> Fields<'_, 'a> {
        Fields { doc: self, at: self.fields.start }
    }
}

/// A walk over a document's fields, each `key value` taken in order.
struct Fields<'d, 'a> {
    doc: &'d Doc<'a>,
    at: usize,
}

impl<'a> Fields<'_, 'a> {
    fn is(&self, key: &str) -> bool {
        self.at < self.doc.fields.end && self.doc.lines[self.at].split_once(' ').is_some_and(|(k, _)| k == key)
    }

    /// The next line's value, where its key is `key`.
    fn take(&mut self, key: &str) -> Result<&'a str, Refused> {
        if self.at >= self.doc.fields.end {
            return Err(self.here("the fields end before one the format requires"));
        }
        match self.doc.lines[self.at].split_once(' ') {
            Some((k, value)) if k == key => {
                self.at += 1;
                Ok(value)
            }
            _ => Err(self.here("a field that is unknown, repeated, missing or out of order")),
        }
    }

    /// [`Self::take`], of a value that is one word.
    fn word(&mut self, key: &str) -> Result<&'a str, Refused> {
        let value = self.take(key)?;
        if value.contains(' ') {
            return Err(self.last("a value that is not one word"));
        }
        Ok(value)
    }

    fn end(&self) -> Result<(), Refused> {
        if self.at < self.doc.fields.end {
            return Err(self.here("a field that is unknown, repeated or out of order"));
        }
        Ok(())
    }

    /// A refusal at the line not yet taken.
    fn here(&self, why: &'static str) -> Refused {
        Refused::Malformed { role: self.doc.role, line: self.at + 1, why }
    }

    /// A refusal at the line last taken.
    fn last(&self, why: &'static str) -> Refused {
        Refused::Malformed { role: self.doc.role, line: self.at, why }
    }
}

/// A decimal with no sign and no leading zero.
fn number(text: &str) -> Option<u64> {
    let canonical = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) && (text == "0" || !text.starts_with('0'));
    if !canonical {
        return None;
    }
    text.parse().ok()
}

fn hex32(text: &str) -> Option<Digest> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = digit(bytes[2 * i])? << 4 | digit(bytes[2 * i + 1])?;
    }
    Some(out)
}

fn is_name(text: &str) -> bool {
    (1..=NAME_MAX).contains(&text.len())
        && !text.starts_with('.')
        && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}

/// `a/b.tar.gz`: components of `A-Z a-z 0-9 . _ + -`, none empty, `.` or
/// `..`, so no path leaves the repository and none names a scheme.
fn is_relative(text: &str) -> bool {
    text.len() <= 255
        && text.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        })
}

/// `YYYY-MM-DDTHH:MM:SSZ`, an instant that exists, in Unix seconds.
pub fn parse_time(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    let shape = b.len() == 20
        && b.iter().enumerate().all(|(i, &c)| match i {
            4 | 7 => c == b'-',
            10 => c == b'T',
            13 | 16 => c == b':',
            19 => c == b'Z',
            _ => c.is_ascii_digit(),
        });
    if !shape {
        return None;
    }
    let field = |at: core::ops::Range<usize>| text[at].parse::<u64>().expect("digits");
    let civil = toyos_wallclock::Civil {
        year: field(0..4),
        month: field(5..7),
        day: field(8..10),
        hour: field(11..13),
        min: field(14..16),
        sec: field(17..19),
    };
    civil.is_valid().then(|| civil.to_unix_secs())
}

/// The form [`parse_time`] reads.
pub fn time_text(secs: u64) -> String {
    let c = toyos_wallclock::Civil::from_unix_secs(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", c.year, c.month, c.day, c.hour, c.min, c.sec)
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// RFC 4648 base64, padded.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, &b)| acc | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// RFC 4648 base64, padded and canonical: what [`base64_encode`] writes and
/// nothing else, so one blob has one spelling.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let pad = bytes.iter().rev().take_while(|&&c| c == b'=').count();
    if !bytes.len().is_multiple_of(4) || pad > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in &bytes[..bytes.len() - pad] {
        acc = acc << 6 | ALPHABET.iter().position(|&a| a == c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (acc == 0).then_some(out)
}

/// The renderer: the publisher's half, beside the parser it has to agree with.
#[cfg(any(test, feature = "sign"))]
pub mod render {
    use super::*;

    /// `root`'s signed bytes, unsigned.
    pub fn root(root: &Root) -> String {
        let mut out = header(Role::Root, root.version, root.expires);
        let mut keys: Vec<(Digest, &PublicKey)> = root.keys.iter().map(|k| (key_id(k), k)).collect();
        keys.sort();
        for (id, key) in keys {
            out += &format!("key {} ssh-ed25519 {}\n", hex(&id), base64_encode(&public_blob(key)));
        }
        for role in Role::ALL {
            let grant = root.grant(role);
            out += &format!("role {} {}", role.name(), grant.threshold);
            for id in &grant.keys {
                out += &format!(" {}", hex(id));
            }
            out.push('\n');
        }
        out
    }

    pub fn timestamp(timestamp: &Timestamp) -> String {
        let t = timestamp.targets;
        header(Role::Timestamp, timestamp.version, timestamp.expires)
            + &format!("targets {} {} {}\n", t.version, t.length, hex(&t.sha256))
    }

    pub fn targets(targets: &Targets) -> String {
        let mut out = header(Role::Targets, targets.version, targets.expires);
        for i in &targets.items {
            out += &format!(
                "item {}\ntarget {}\nsequence {}\nversion {}\nurl {}\nlength {}\nsha256 {}\n",
                i.name,
                i.target,
                i.sequence,
                i.version,
                i.url,
                i.length,
                hex(&i.sha256)
            );
        }
        out
    }

    /// The `sig` line `seed` signs `body` with, as `role`.
    pub fn signature(seed: &[u8; 32], role: Role, body: &[u8]) -> String {
        let public = sig::public_of(seed);
        let signature = sig::sign(seed, &Body { role, bytes: body });
        let mut blob = b"SSHSIG".to_vec();
        blob.extend_from_slice(&1u32.to_be_bytes());
        let mut inner = Vec::new();
        put(&mut inner, ED25519);
        put(&mut inner, &signature);
        for field in [&public_blob(&public)[..], role.namespace().as_bytes(), b"", b"sha512", &inner] {
            put(&mut blob, field);
        }
        format!("sig {} {}\n", hex(&key_id(&public)), base64_encode(&blob))
    }

    fn put(out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(bytes);
    }

    fn header(role: Role, version: u64, expires: u64) -> String {
        format!("{MAGIC} {} {version}\nexpires {}\n", role.name(), time_text(expires))
    }

    pub fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::render;
    use super::*;
    use alloc::collections::BTreeMap;

    /// 2026-10-09T00:00:00Z: every test's wall clock.
    const NOW: u64 = 1_791_504_000;
    const DAY: u64 = 86_400;
    const TRIPLE: &str = "x86_64-unknown-toyos";
    const ARCHIVE: &[u8] = b"the archive's bytes";

    fn seed(n: u8) -> [u8; 32] {
        [n; 32]
    }

    fn public(n: u8) -> PublicKey {
        sig::public_of(&seed(n))
    }

    fn signed(body: String, role: Role, signers: &[u8]) -> Vec<u8> {
        let mut out = body.clone();
        for &n in signers {
            out += &render::signature(&seed(n), role, body.as_bytes());
        }
        out.into_bytes()
    }

    /// A root whose roles are signed by the key numbers given, each at
    /// `threshold`.
    fn root(version: u64, roles: [(usize, &[u8]); 3]) -> Root {
        let mut all: Vec<u8> = roles.iter().flat_map(|(_, k)| k.iter().copied()).collect();
        all.sort();
        all.dedup();
        let grants = roles.map(|(threshold, keys)| {
            let mut keys: Vec<Digest> = keys.iter().map(|&n| key_id(&public(n))).collect();
            keys.sort();
            Grant { threshold, keys }
        });
        Root { version, expires: NOW + 365 * DAY, keys: all.iter().map(|&n| public(n)).collect(), grants }
    }

    fn one_key(version: u64, n: u8) -> Root {
        root(version, [(1, &[n]), (1, &[n]), (1, &[n])])
    }

    fn item(sequence: u64, archive: &[u8]) -> Item {
        Item {
            name: "gbae".into(),
            target: TRIPLE.into(),
            sequence,
            version: "0.2.0".into(),
            url: "archives/gbae-v0.2.0-toyos-x86_64.tar.gz".into(),
            length: archive.len() as u64,
            sha256: sha256(archive),
        }
    }

    fn targets(version: u64, items: Vec<Item>) -> Targets {
        Targets { version, expires: NOW + 90 * DAY, items }
    }

    fn timestamp_of(version: u64, targets: &[u8]) -> Timestamp {
        let snapshot = Targets::parse(targets).expect("a targets").version;
        Timestamp {
            version,
            expires: NOW + 7 * DAY,
            targets: Snapshot { version: snapshot, length: targets.len() as u64, sha256: sha256(targets) },
        }
    }

    /// A repository in memory, and the files the client asked it for.
    #[derive(Default, Clone)]
    struct Repo(BTreeMap<String, Vec<u8>>);

    impl Mirror for Repo {
        fn fetch(&mut self, name: &str, cap: usize) -> Result<Option<Vec<u8>>, String> {
            Ok(self.0.get(name).map(|b| b[..b.len().min(cap + 1)].to_vec()))
        }
    }

    impl Repo {
        fn put(&mut self, name: &str, bytes: Vec<u8>) {
            self.0.insert(name.into(), bytes);
        }

        /// A targets and the timestamp naming it, both signed by `n`.
        fn publish(&mut self, timestamp: u64, t: &Targets, n: u8) -> Vec<u8> {
            let bytes = signed(render::targets(t), Role::Targets, &[n]);
            let ts = signed(render::timestamp(&timestamp_of(timestamp, &bytes)), Role::Timestamp, &[n]);
            self.put(&targets_file(t.version), bytes.clone());
            self.put(TIMESTAMP_FILE, ts);
            bytes
        }
    }

    /// The image pins root 1, timestamp 1 and targets 1, all key 1's; the
    /// repository serves timestamp 2 naming targets 2, with gbae at sequence 3.
    struct World {
        image_root: Vec<u8>,
        image_timestamp: Vec<u8>,
        image_targets: Vec<u8>,
        repo: Repo,
    }

    impl World {
        fn new() -> Self {
            let mut repo = Repo::default();
            let image_root = signed(render::root(&one_key(1, 1)), Role::Root, &[1]);
            let image_targets = repo.publish(1, &targets(1, vec![item(3, ARCHIVE)]), 1);
            let image_timestamp = repo.0[TIMESTAMP_FILE].clone();
            repo.publish(2, &targets(2, vec![item(3, ARCHIVE)]), 1);
            World { image_root, image_timestamp, image_targets, repo }
        }

        fn image(&self) -> Held<'_> {
            Held { root: &self.image_root, timestamp: Some(&self.image_timestamp), targets: Some(&self.image_targets) }
        }

        /// The repository and the image's copies, borrowed apart.
        fn parts(&mut self) -> (&mut Repo, Held<'_>) {
            let image = Held { root: &self.image_root, timestamp: Some(&self.image_timestamp), targets: Some(&self.image_targets) };
            (&mut self.repo, image)
        }

        fn refresh(&mut self) -> Result<Fresh, Refused> {
            let (repo, image) = self.parts();
            refresh(repo, image, None, NOW)
        }
    }

    /// The refusal, and never a print of what was accepted.
    fn refused(result: Result<Fresh, Refused>) -> Refused {
        match result {
            Ok(_) => panic!("the client accepted what it must refuse"),
            Err(why) => why,
        }
    }

    #[test]
    fn a_repository_the_renderer_writes_is_one_the_client_accepts() {
        let mut world = World::new();
        let fresh = world.refresh().expect("the repository");
        let gbae = fresh.targets.find("gbae", TRIPLE).expect("gbae");
        assert_eq!((gbae.sequence, gbae.length), (3, ARCHIVE.len() as u64));
        assert_eq!(fresh.root, world.image_root, "no root past the image's");
        assert_eq!(fresh.targets_bytes, world.repo.0["targets.2.txt"]);
        let mut archive = Archive::of(gbae);
        archive.take(&ARCHIVE[..5]).unwrap();
        archive.take(&ARCHIVE[5..]).unwrap();
        assert_eq!(archive.finish(), Ok(()));

        // What the machine then holds is a floor and not a refusal: the same
        // repository fetched again is accepted.
        let machine = Held { root: &fresh.root, timestamp: Some(&fresh.timestamp), targets: Some(&fresh.targets_bytes) };
        let (repo, image) = world.parts();
        refresh(repo, image, Some(machine), NOW).expect("the same repository again");
    }

    /// **The negative control's target.** Two keys, threshold two, and the
    /// one key's signature twice: one signature, which does not meet two.
    #[test]
    fn a_duplicated_signature_does_not_meet_a_threshold() {
        let mut world = World::new();
        let two = root(2, [(1, &[1]), (1, &[1]), (2, &[1, 2])]);
        world.repo.put("root.2.txt", signed(render::root(&two), Role::Root, &[1]));
        let t = targets(2, vec![item(3, ARCHIVE)]);
        let body = render::targets(&t);
        let line = render::signature(&seed(1), Role::Targets, body.as_bytes());
        let bytes = format!("{body}{line}{line}").into_bytes();
        let ts = signed(render::timestamp(&timestamp_of(2, &bytes)), Role::Timestamp, &[1]);
        world.repo.put("targets.2.txt", bytes);
        world.repo.put(TIMESTAMP_FILE, ts);
        assert_eq!(
            refused(world.refresh()),
            Refused::Threshold { role: Role::Targets, version: 2, root: 2, valid: 1, needed: 2, first: None }
        );

        // Both keys: met.
        let both = signed(body, Role::Targets, &[1, 2]);
        let ts = signed(render::timestamp(&timestamp_of(2, &both)), Role::Timestamp, &[1]);
        world.repo.put("targets.2.txt", both);
        world.repo.put(TIMESTAMP_FILE, ts);
        world.refresh().expect("two distinct keys meet two");
    }

    /// A signature the role's own key made over the very bytes, in another
    /// namespace — a timestamp's, or an image's — vouches for nothing here.
    #[test]
    fn a_signature_in_another_namespace_is_refused() {
        let mut world = World::new();
        let body = render::targets(&targets(2, vec![item(3, ARCHIVE)]));
        let as_timestamp = render::signature(&seed(1), Role::Timestamp, body.as_bytes());
        let bytes = format!("{body}{as_timestamp}").into_bytes();
        let ts = signed(render::timestamp(&timestamp_of(2, &bytes)), Role::Timestamp, &[1]);
        world.repo.put("targets.2.txt", bytes);
        world.repo.put(TIMESTAMP_FILE, ts);
        assert_eq!(
            refused(world.refresh()),
            Refused::Threshold { role: Role::Targets, version: 2, root: 1, valid: 0, needed: 1, first: Some(SigRefused::Namespace) }
        );

        // The blob relabelled into the right namespace still carries the
        // signature made in the other: the bytes signed differ.
        let blob = base64_decode(as_timestamp.trim_end().rsplit(' ').next().unwrap()).unwrap();
        let at =blob.windows(15).position(|w| w == b"toyos-timestamp").unwrap();
        let mut moved = blob[..at - 4].to_vec();
        moved.extend_from_slice(&13u32.to_be_bytes());
        moved.extend_from_slice(b"toyos-targets");
        moved.extend_from_slice(&blob[at + 15..]);
        let line = format!("sig {} {}\n", render::hex(&key_id(&public(1))), base64_encode(&moved));
        let bytes = format!("{body}{line}").into_bytes();
        let ts = signed(render::timestamp(&timestamp_of(2, &bytes)), Role::Timestamp, &[1]);
        world.repo.put("targets.2.txt", bytes);
        world.repo.put(TIMESTAMP_FILE, ts);
        assert!(matches!(
            refused(world.refresh()),
            Refused::Threshold { first: Some(SigRefused::Signature), valid: 0, .. }
        ));

        // And the image's verifier refuses a repository signature: the
        // namespaces are the things' own.
        let header = [0u8; crate::image::HEADER_BYTES];
        let as_targets = sig::sign(&seed(1), &Body { role: Role::Targets, bytes: &header });
        assert_eq!(sig::verify(&public(1), &header, &as_targets), Err(sig::Refused::Signature));
    }

    #[test]
    fn a_role_version_below_the_held_one_is_refused() {
        let mut world = World::new();
        let fresh = world.refresh().unwrap();
        let machine_ts = fresh.timestamp.clone();
        let machine_targets = fresh.targets_bytes.clone();
        // The repository rolls back to timestamp 1.
        world.repo.put(TIMESTAMP_FILE, world.image_timestamp.clone());
        let machine = Held { root: &fresh.root, timestamp: Some(&machine_ts), targets: Some(&machine_targets) };
        assert_eq!(
            refused(refresh(&mut world.repo.clone(), world.image(), Some(machine), NOW)),
            Refused::Rollback { role: Role::Timestamp, version: 1, floor: 2, holder: Holder::Machine }
        );

        // A newer timestamp naming the targets below the image's own.
        let mut repo = World::new().repo;
        let old = repo.0["targets.2.txt"].clone();
        let ts = signed(render::timestamp(&timestamp_of(3, &old)), Role::Timestamp, &[1]);
        repo.put(TIMESTAMP_FILE, ts);
        let image_targets = signed(render::targets(&targets(3, vec![item(3, ARCHIVE)])), Role::Targets, &[1]);
        let image = Held { targets: Some(&image_targets), ..world.image() };
        assert_eq!(
            refused(refresh(&mut repo, image, None, NOW)),
            Refused::Rollback { role: Role::Targets, version: 2, floor: 3, holder: Holder::Image }
        );
    }

    #[test]
    fn the_same_version_with_other_bytes_is_refused() {
        let mut world = World::new();
        // Timestamp 1 again, re-signed with another expiry: the version the
        // image holds, and not its bytes.
        let image_targets = world.image_targets.clone();
        let mut again = timestamp_of(1, &image_targets);
        again.expires += 1;
        world.repo.put(TIMESTAMP_FILE, signed(render::timestamp(&again), Role::Timestamp, &[1]));
        assert_eq!(
            refused(world.refresh()),
            Refused::Changed { role: Role::Timestamp, version: 1, holder: Holder::Image }
        );

        // Targets 1 again, another body under the version the image holds.
        let mut world = World::new();
        world.repo.publish(2, &targets(1, vec![item(4, ARCHIVE)]), 1);
        assert_eq!(refused(world.refresh()), Refused::Changed { role: Role::Targets, version: 1, holder: Holder::Image });
    }

    #[test]
    fn expired_metadata_is_refused_with_both_times() {
        let mut world = World::new();
        let mut late = World::new();
        let then = Timestamp::parse(&world.repo.0[TIMESTAMP_FILE]).unwrap().expires;
        let (repo, image) = world.parts();
        let why = refused(refresh(repo, image, None, then));
        assert_eq!(why, Refused::Expired { role: Role::Timestamp, version: 2, expires: then, now: then });
        assert!(why.to_string().contains(&time_text(then)), "{why}");
        late.repo.publish(3, &Targets { expires: NOW - 1, ..targets(2, vec![item(3, ARCHIVE)]) }, 1);
        assert!(matches!(refused(late.refresh()), Refused::Expired { role: Role::Targets, version: 2, .. }));
        let root_expiry = Root::parse(&late.image_root).unwrap().expires;
        assert!(matches!(
            refused(refresh(&mut late.repo, World::new().image(), None, root_expiry)),
            Refused::Expired { role: Role::Root, version: 1, .. }
        ));
    }

    #[test]
    fn targets_not_matching_the_timestamp_are_refused() {
        let mut world = World::new();
        let mut bytes = world.repo.0["targets.2.txt"].clone();
        bytes.push(b'\n');
        world.repo.put("targets.2.txt", bytes.clone());
        let length = bytes.len() as u64 - 1;
        assert_eq!(refused(world.refresh()), Refused::TargetsLength { want: length, got: length + 1 });
        bytes.pop();
        let last = bytes.len() - 2;
        bytes[last] ^= 1;
        world.repo.put("targets.2.txt", bytes);
        assert_eq!(refused(world.refresh()), Refused::TargetsDigest { version: 2 });
    }

    #[test]
    fn an_archive_past_its_length_or_not_its_hash_is_refused() {
        let gbae = item(3, ARCHIVE);
        let mut past = Archive::of(&gbae);
        past.take(ARCHIVE).unwrap();
        assert_eq!(past.take(b"x"), Err(Refused::ArchivePast { length: ARCHIVE.len() as u64 }));
        let mut long = Archive::of(&gbae);
        assert_eq!(long.take(&[ARCHIVE, b"x"].concat()), Err(Refused::ArchivePast { length: ARCHIVE.len() as u64 }));

        let mut bent = ARCHIVE.to_vec();
        bent[0] ^= 1;
        let mut wrong = Archive::of(&gbae);
        wrong.take(&bent).unwrap();
        assert_eq!(wrong.finish(), Err(Refused::ArchiveDigest));
        let mut short = Archive::of(&gbae);
        short.take(&ARCHIVE[..3]).unwrap();
        assert_eq!(short.finish(), Err(Refused::ArchiveShort { length: ARCHIVE.len() as u64, got: 3 }));
    }

    #[test]
    fn a_lower_sequence_or_a_reissued_one_is_refused() {
        let mut world = World::new();
        world.repo.publish(2, &targets(2, vec![item(2, ARCHIVE)]), 1);
        assert_eq!(
            refused(world.refresh()),
            Refused::Sequence { name: "gbae".into(), target: TRIPLE.into(), sequence: 2, floor: 3, holder: Holder::Image }
        );
        let mut world = World::new();
        world.repo.publish(2, &targets(2, vec![item(3, b"other bytes")]), 1);
        assert_eq!(
            refused(world.refresh()),
            Refused::Reissued { name: "gbae".into(), target: TRIPLE.into(), sequence: 3, holder: Holder::Image }
        );
        let mut world = World::new();
        world.repo.publish(2, &targets(2, vec![item(4, b"other bytes")]), 1);
        world.refresh().expect("a higher sequence with other bytes");
    }

    #[test]
    fn a_root_not_signed_by_the_previous_threshold_is_refused() {
        // Root 2 hands every role to key 2, signed by key 2 alone.
        let mut world = World::new();
        let two = one_key(2, 2);
        world.repo.put("root.2.txt", signed(render::root(&two), Role::Root, &[2]));
        assert_eq!(
            refused(world.refresh()),
            Refused::Threshold { role: Role::Root, version: 2, root: 1, valid: 0, needed: 1, first: Some(SigRefused::NotTheRolesKey) }
        );
        // Signed by key 1 alone: not by its own threshold.
        world.repo.put("root.2.txt", signed(render::root(&two), Role::Root, &[1]));
        assert_eq!(
            refused(world.refresh()),
            Refused::Threshold { role: Role::Root, version: 2, root: 2, valid: 0, needed: 1, first: Some(SigRefused::NotTheRolesKey) }
        );
        // A root that calls itself another version.
        world.repo.put("root.2.txt", signed(render::root(&one_key(3, 1)), Role::Root, &[1]));
        assert_eq!(refused(world.refresh()), Refused::RootVersion { want: 2, got: 3 });
    }

    /// Signed by both: the rotation is taken, the image's floors were key 1's
    /// and count no more, so a repository the stolen key fast-forwarded is
    /// left behind; and the new key's own documents are accepted.
    #[test]
    fn a_rotated_root_is_walked_and_drops_the_old_keys_floors() {
        let mut world = World::new();
        let two = one_key(2, 2);
        world.repo.put("root.2.txt", signed(render::root(&two), Role::Root, &[1, 2]));
        assert!(matches!(
            refused(world.refresh()),
            Refused::Threshold { role: Role::Timestamp, root: 2, first: Some(SigRefused::NotTheRolesKey), .. }
        ));
        world.repo.publish(1, &targets(1, vec![item(1, ARCHIVE)]), 2);
        let fresh = world.refresh().expect("the rotated repository from version 1");
        assert_eq!(Root::parse(&fresh.root).unwrap(), two);
    }

    /// The machine holds root 2, which took every role from key 1 for key 2;
    /// a mirror that withholds `root.2.txt` and serves key 1's timestamp is
    /// still judged by root 2, never by the image's older root 1.
    #[test]
    fn a_machines_newer_root_holds_though_the_mirror_withholds_it() {
        let mut world = World::new();
        let two = signed(render::root(&one_key(2, 2)), Role::Root, &[1, 2]);
        let machine = Held { root: &two, timestamp: None, targets: None };
        let (repo, image) = world.parts();
        assert!(matches!(
            refused(refresh(repo, image, Some(machine), NOW)),
            Refused::Threshold { role: Role::Timestamp, root: 2, first: Some(SigRefused::NotTheRolesKey), .. }
        ));
    }

    #[test]
    fn a_machines_root_at_the_images_version_with_other_bytes_is_refused() {
        let mut world = World::new();
        let other = signed(render::root(&Root { expires: NOW + DAY, ..one_key(1, 1) }), Role::Root, &[1]);
        assert_ne!(other, world.image_root);
        let machine = Held { root: &other, timestamp: None, targets: None };
        let (repo, image) = world.parts();
        assert_eq!(
            refused(refresh(repo, image, Some(machine), NOW)),
            Refused::Changed { role: Role::Root, version: 1, holder: Holder::Image }
        );
    }

    /// No document is at the last version, so a walk always has a next one to
    /// ask for: a held root there is not a root, and a mirror's is refused by
    /// its header.
    #[test]
    fn a_root_at_the_last_version_is_refused() {
        let mut world = World::new();
        let last = signed(render::root(&one_key(u64::MAX, 1)), Role::Root, &[1]);
        let machine = Held { root: &last, timestamp: None, targets: None };
        let (repo, image) = world.parts();
        assert_eq!(
            refused(refresh(repo, image, Some(machine), NOW)),
            Refused::Held { role: Role::Root, holder: Holder::Machine }
        );

        let mut world = World::new();
        let before = signed(render::root(&one_key(u64::MAX - 1, 1)), Role::Root, &[1]);
        world.repo.put(&root_file(u64::MAX), last.clone());
        let machine = Held { root: &before, timestamp: None, targets: None };
        let (repo, image) = world.parts();
        assert!(matches!(
            refused(refresh(repo, image, Some(machine), NOW)),
            Refused::Malformed { role: Role::Root, line: 1, .. }
        ));
    }

    #[test]
    fn a_chain_past_the_walk_bound_is_refused() {
        let mut world = World::new();
        for v in 2..=ROOT_STEPS + 2 {
            world.repo.put(&root_file(v), signed(render::root(&one_key(v, 1)), Role::Root, &[1]));
        }
        assert_eq!(refused(world.refresh()), Refused::RootChain);
        world.repo.0.remove(&root_file(ROOT_STEPS + 2));
        assert_eq!(Root::parse(&world.refresh().unwrap().root).unwrap().version, ROOT_STEPS + 1);
    }

    #[test]
    fn a_document_past_its_cap_is_refused_unread() {
        let mut world = World::new();
        let body = render::timestamp(&Timestamp::parse(&world.repo.0[TIMESTAMP_FILE]).unwrap());
        let mut bytes = signed(body, Role::Timestamp, &[1]);
        bytes.resize(TIMESTAMP_CAP + 4096, b'x');
        world.repo.put(TIMESTAMP_FILE, bytes);
        assert_eq!(refused(world.refresh()), Refused::TooLarge { role: Role::Timestamp, cap: TIMESTAMP_CAP });
        let mut world = World::new();
        world.repo.put("root.2.txt", vec![b'k'; ROOT_CAP + 1]);
        assert_eq!(refused(world.refresh()), Refused::TooLarge { role: Role::Root, cap: ROOT_CAP });
    }

    /// Every bend of a valid document a parser could be lenient about.
    #[test]
    fn a_document_that_is_not_the_format_is_refused_by_line() {
        let body = render::targets(&targets(2, vec![item(3, ARCHIVE)]));
        let good = signed(body.clone(), Role::Targets, &[1]);
        Targets::parse(&good).expect("the unbent document");
        let bend = |from: &str, to: &str| {
            assert!(body.contains(from), "{from:?}");
            let text = String::from_utf8(good.clone()).unwrap().replacen(from, to, 1);
            Targets::parse(text.as_bytes()).expect_err(&format!("{from:?} -> {to:?}"))
        };
        let line = |r: Refused| match r {
            Refused::Malformed { line, .. } => line,
            other => panic!("not a format refusal: {other}"),
        };
        assert_eq!(line(bend("toyos-repo targets 2", "toyos-repo timestamp 2")), 1);
        assert_eq!(line(bend("toyos-repo targets 2", "toyos-repo targets 02")), 1);
        assert_eq!(line(bend("-01-07T", "-02-30T")), 2);
        assert_eq!(line(bend("T00:00:00Z", "T24:00:00Z")), 2);
        assert_eq!(line(bend("sequence 3\n", "sequence 3\nsequence 3\n")), 6);
        assert_eq!(line(bend("version 0.2.0\n", "")), 6);
        assert_eq!(line(bend("version 0.2.0\n", "version 0.2.0\nowner me\n")), 7);
        assert_eq!(line(bend("item gbae", "item Gbae")), 3);
        assert_eq!(line(bend("url archives/", "url ../")), 7);
        assert_eq!(line(bend("url archives/", "url https://example.org/")), 7);
        assert_eq!(line(bend("length ", "length 0")), 8);
        assert_eq!(line(bend("\n", "\r\n")), 1);
        assert_eq!(line(bend("version 0.2.0", "version 0.2.0 beta")), 6);
        assert_eq!(line(bend("version 0.2.0", "version  0.2.0")), 6);
        let mut unended = good.clone();
        unended.pop();
        assert!(matches!(Targets::parse(&unended), Err(Refused::Malformed { .. })));
        let after = [good.clone(), b"item late\n".to_vec()].concat();
        assert!(matches!(Targets::parse(&after), Err(Refused::Malformed { .. })));

        // Items in order, once each.
        let twice = render::targets(&targets(2, vec![item(3, ARCHIVE), item(3, ARCHIVE)]));
        assert!(matches!(Targets::parse(twice.as_bytes()), Err(Refused::Malformed { line: 16, .. })));

        // A root whose key line is not its key's ID, a threshold past its
        // keys, roles out of order.
        let r = render::root(&root(1, [(1, &[1]), (1, &[1]), (1, &[1, 2])]));
        let id1 = render::hex(&key_id(&public(1)));
        let id2 = render::hex(&key_id(&public(2)));
        let rbend = |from: &str, to: &str| {
            assert!(r.contains(from), "{from:?}");
            Root::parse(r.replacen(from, to, 1).as_bytes()).expect_err(&format!("{from:?} -> {to:?}"))
        };
        Root::parse(r.as_bytes()).expect("the unbent root");
        assert!(matches!(rbend(&format!("key {id1}"), &format!("key {id2}")), Refused::Malformed { .. }));
        assert!(matches!(rbend("role targets 1", "role targets 3"), Refused::Malformed { .. }));
        assert!(matches!(rbend("role root", "role timestamp"), Refused::Malformed { line: 5, .. }));
        assert!(matches!(rbend(&format!("role timestamp 1 {id1}"), &format!("role timestamp 1 {}", "0".repeat(64))), Refused::Malformed { .. }));
    }

    #[test]
    fn base64_has_one_spelling() {
        for bytes in [&b""[..], b"M", b"Ma", b"Man", b"any carnal pleas"] {
            assert_eq!(base64_decode(&base64_encode(bytes)).as_deref(), Some(bytes));
        }
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        for bent in ["TWF", "TWE", "TWF=", "TW==x", "TQ=", "TR==", "T===", "TWE=\n", "TW E="] {
            assert_eq!(base64_decode(bent), None, "{bent:?}");
        }
    }

    #[test]
    fn a_time_is_one_that_exists_and_reads_back() {
        assert_eq!(parse_time("2026-10-09T00:00:00Z"), Some(NOW));
        assert_eq!(time_text(NOW), "2026-10-09T00:00:00Z");
        for bad in ["2026-10-09 00:00:00Z", "2026-10-09T00:00:00", "2026-13-01T00:00:00Z", "2027-02-29T00:00:00Z", "+026-10-09T00:00:00Z"] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
    }
}
