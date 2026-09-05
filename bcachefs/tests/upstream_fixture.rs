//! The upstream read path, judged against volumes upstream's own tools wrote.
//!
//! **The bytes are the judge, and nothing fetches them.** `bcachefs-tools`
//! formatted and filled each fixture under `fixtures/`, and `bcachefs fsck`
//! called it clean; `NOTICE` records which release did it and the commands
//! that were run. What is committed is the result, so this suite depends on
//! Rust and nothing else — the tools are a development instrument and no part
//! of the tree.
//!
//! The contents are this project's own test bytes, chosen so the host can
//! predict every one of them without asking anything: an eleven-byte file that
//! bcachefs stores inline, `seq 1 40000` spanning several checksummed extents,
//! an empty directory, a nested directory and a symlink.

use std::io::Read;
use std::path::{Path, PathBuf};

use bcachefs::upstream::fs::{FileKind, Volume};
use bcachefs::{BlockBuf, BlockIO, BlockNum, DeviceError, TransferError};

/// The eleven-byte file, short enough that bcachefs keeps it in the btree
/// rather than in an extent.
const HELLO: &str = "hello-toyos";
const LINK_TARGET: &str = "../a.txt";
/// The line count of the multi-extent file, and what makes it predictable.
const SEQ_LINES: u32 = 40_000;

/// A fixture as a read-only block device.
///
/// The volume is decompressed into memory: it is 16 MB of which almost all is
/// zeros, and holding it as bytes keeps the committed artifact at a hundred and
/// some kilobytes.
struct FixtureIo {
    bytes: Vec<u8>,
}

struct HostIoFailed;
impl TransferError for HostIoFailed {
    fn refused_before_attempt(&self) -> bool {
        false
    }
}

fn failed() -> DeviceError {
    DeviceError::classify(&HostIoFailed)
}

impl BlockIO for FixtureIo {
    fn read_block(&self, block: BlockNum, buf: &mut BlockBuf) -> Result<(), DeviceError> {
        let at = (block.raw() as usize).checked_mul(4096).ok_or_else(failed)?;
        let end = at.checked_add(4096).ok_or_else(failed)?;
        buf.as_bytes_mut().copy_from_slice(self.bytes.get(at..end).ok_or_else(failed)?);
        Ok(())
    }

    fn write_block(&self, _block: BlockNum, _buf: &BlockBuf) -> Result<(), DeviceError> {
        Err(failed())
    }

    fn block_count(&self) -> u64 {
        (self.bytes.len() / 4096) as u64
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn open(name: &str) -> Volume<FixtureIo> {
    let path = fixture(name);
    let gz = std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(&gz[..])
        .read_to_end(&mut bytes)
        .unwrap_or_else(|e| panic!("decompressing {}: {e}", path.display()));
    Volume::open(FixtureIo { bytes })
        .unwrap_or_else(|e| panic!("opening the volume in {}: {e:?}", path.display()))
}

/// `seq 1 40000`'s output, which the tools wrote into the fixture and this
/// side reproduces without reading it.
fn seq_text() -> Vec<u8> {
    let mut out = Vec::new();
    for n in 1..=SEQ_LINES {
        out.extend_from_slice(n.to_string().as_bytes());
        out.push(b'\n');
    }
    out
}

/// Every entry of a directory, as `<kind> <name>`, sorted.
fn listing(volume: &Volume<FixtureIo>, dir: u64) -> Vec<String> {
    let mut out = Vec::new();
    volume
        .readdir(dir, &mut |name, _, kind| {
            let letter = match kind {
                FileKind::Dir => 'd',
                FileKind::Regular => 'f',
                FileKind::Symlink => 'l',
                FileKind::Other(_) => '?',
            };
            out.push(format!("{letter} {name}"));
            true
        })
        .expect("listing a directory");
    out.sort();
    out
}

fn resolve(volume: &Volume<FixtureIo>, path: &str) -> (u64, FileKind) {
    volume
        .resolve(path)
        .unwrap_or_else(|e| panic!("resolving {path}: {e:?}"))
        .unwrap_or_else(|| panic!("the fixture holds no {path}"))
}

/// The whole tree, read back out of a volume upstream wrote.
///
/// This is the judgement the suite used to make by booting a Linux
/// distribution: same assertions, against bytes that are committed.
#[test]
fn the_upstream_volume_reads_back_as_what_upstream_wrote() {
    let volume = open("crc32c.img.gz");

    assert_eq!(
        listing(&volume, volume.root()),
        ["d Documents", "d empty", "d lost+found"],
        "the root is not what the tools put there"
    );

    let (documents, kind) = resolve(&volume, "/Documents");
    assert_eq!(kind, FileKind::Dir);
    assert_eq!(listing(&volume, documents), ["d deep", "f a.txt"]);

    let (deep, kind) = resolve(&volume, "/Documents/deep");
    assert_eq!(kind, FileKind::Dir, "the nested directory");
    assert_eq!(listing(&volume, deep), ["f seq.txt", "l link"]);

    // An empty directory reads back as one, rather than as absent.
    let (empty, kind) = resolve(&volume, "/empty");
    assert_eq!(kind, FileKind::Dir);
    assert!(listing(&volume, empty).is_empty(), "the empty directory listed something");

    // The inline-data file: short enough that its bytes are in the btree.
    let (a_txt, kind) = resolve(&volume, "/Documents/a.txt");
    assert_eq!(kind, FileKind::Regular);
    let attrs = volume.stat(a_txt).expect("stat").expect("an inode");
    assert_eq!(attrs.size, HELLO.len() as u64);
    assert_eq!(attrs.mode & 0o170_000, 0o100_000, "a.txt is not a regular file");
    assert_eq!(volume.read(a_txt).expect("reading a.txt"), HELLO.as_bytes());

    // The multi-extent file, byte for byte against a copy this side made.
    let (seq_txt, kind) = resolve(&volume, "/Documents/deep/seq.txt");
    assert_eq!(kind, FileKind::Regular);
    let want = seq_text();
    let attrs = volume.stat(seq_txt).expect("stat").expect("an inode");
    assert_eq!(attrs.size, want.len() as u64, "seq.txt is not the length it was written at");
    assert_eq!(volume.read(seq_txt).expect("reading seq.txt"), want);

    let (link, kind) = resolve(&volume, "/Documents/deep/link");
    assert_eq!(kind, FileKind::Symlink);
    assert_eq!(volume.read_link(link).expect("reading the symlink"), LINK_TARGET);

    // The root directory is the one the format's own constant names.
    assert_eq!(volume.root(), 4096);
}

/// A path the volume does not hold is `None`, not an error and not a guess.
#[test]
fn a_path_the_volume_does_not_hold_reads_back_as_absent() {
    let volume = open("crc32c.img.gz");
    for path in ["/nope", "/Documents/nope", "/Documents/a.txt/under-a-file", "/empty/nope"] {
        assert_eq!(volume.resolve(path).expect("resolving"), None, "{path} resolved to something");
    }
}
