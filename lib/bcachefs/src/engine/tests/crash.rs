//! The read-write mount stopped at every block request: what a crash there
//! leaves replays to a prefix of the operations, holding every one whose
//! commit returned, and checks clean.

use std::boxed::Box;
use std::cell::Cell;
use std::collections::BTreeSet;
use std::string::String;
use std::vec::Vec;

use toyos_blockdev::executor::Executor;
use toyos_blockdev::model::{Model, Overlay};
use toyos_blockdev::{block_on_ready, Block, BlockDev, Bytes, ReadOnlyBytes};

use super::mount::{random, MEMORY, NOW, OWNER};
use super::*;

/// A device that, after each write and flush it passes on, hands the model
/// to `after`.
pub(super) struct Watched<'a> {
    pub model: &'a Model,
    pub after: Box<dyn Fn(&Model) + 'a>,
}

impl BlockDev for Watched<'_> {
    fn blocks(&self) -> u64 {
        self.model.blocks()
    }

    async fn read(&self, lba: u64, out: &mut [Block]) -> core::result::Result<(), toyos_blockdev::Error> {
        self.model.read(lba, out).await
    }

    async fn write(&self, lba: u64, data: &[Block]) -> core::result::Result<(), toyos_blockdev::Error> {
        self.model.write(lba, data).await?;
        (self.after)(self.model);
        Ok(())
    }

    async fn flush(&self) -> core::result::Result<(), toyos_blockdev::Error> {
        self.model.flush().await?;
        (self.after)(self.model);
        Ok(())
    }
}

const MIN_LIMITS: toyos_crypto::scrypt::Limits = toyos_crypto::scrypt::Limits { memory: 1 << 20, work: 1 << 20 };

/// One namespace operation in the root directory.
#[derive(Clone)]
pub(super) enum Op {
    Create(Vec<u8>),
    Mkdir(Vec<u8>),
    /// A second name for the first.
    Link(Vec<u8>, Vec<u8>),
    /// The first name moved to the second, replacing what is there.
    Rename(Vec<u8>, Vec<u8>),
    Unlink(Vec<u8>),
    Rmdir(Vec<u8>),
    /// A file with no name, never released: an orphan at the crash.
    Unnamed,
}

/// The root's names after each prefix of `ops`: `[0]` before any. A rename
/// between two names of one node does nothing, as POSIX has it.
fn names_after(ops: &[Op]) -> Vec<BTreeSet<Vec<u8>>> {
    // Each name and the node it holds, numbered as made.
    let mut now: std::collections::BTreeMap<Vec<u8>, usize> = std::collections::BTreeMap::new();
    let mut out = alloc::vec![BTreeSet::new()];
    for (i, op) in ops.iter().enumerate() {
        match op {
            Op::Create(n) | Op::Mkdir(n) => {
                now.insert(n.clone(), i);
            }
            Op::Link(a, b) => {
                now.insert(b.clone(), now[a]);
            }
            Op::Rename(a, b) => {
                if now.get(b) != Some(&now[a]) {
                    let node = now.remove(a).unwrap();
                    now.insert(b.clone(), node);
                }
            }
            Op::Unlink(n) | Op::Rmdir(n) => {
                now.remove(n);
            }
            Op::Unnamed => {}
        }
        out.push(now.keys().cloned().collect());
    }
    out
}

/// A mixed workload of `n` operations, each valid where it lands: files
/// and directories made, files linked and renamed over one another, files
/// unlinked, empty directories removed, unnamed files made.
pub(super) fn mixed(n: usize) -> Vec<Op> {
    let (mut files, mut dirs): (Vec<Vec<u8>>, Vec<Vec<u8>>) = (Vec::new(), Vec::new());
    let mut ops = Vec::new();
    for i in 0..n {
        let name = format!("n{i}").into_bytes();
        let op = match i % 7 {
            0 | 1 => Op::Create(name),
            2 => Op::Mkdir(name),
            3 if !files.is_empty() => Op::Link(files[i % files.len()].clone(), name),
            4 if files.len() > 1 => Op::Rename(files[0].clone(), files[files.len() - 1].clone()),
            5 if !files.is_empty() => Op::Unlink(files[i % files.len()].clone()),
            6 if !dirs.is_empty() && i % 2 == 0 => Op::Rmdir(dirs[i % dirs.len()].clone()),
            6 => Op::Unnamed,
            _ => Op::Create(name),
        };
        match &op {
            Op::Create(n) | Op::Link(_, n) => files.push(n.clone()),
            Op::Mkdir(n) => dirs.push(n.clone()),
            Op::Rename(a, b) => {
                files.retain(|f| f != a);
                if !files.contains(b) {
                    files.push(b.clone());
                }
            }
            Op::Unlink(n) => files.retain(|f| f != n),
            Op::Rmdir(n) => dirs.retain(|d| d != n),
            Op::Unnamed => {}
        }
        ops.push(op);
    }
    ops
}

pub(super) async fn apply<D: BlockDev>(m: &Mount<D>, op: &Op) {
    let r = m.root();
    match op {
        Op::Create(n) => drop(m.create(r, n, New::File, OWNER, NOW).await.unwrap()),
        Op::Mkdir(n) => drop(m.create(r, n, New::Dir, OWNER, NOW).await.unwrap()),
        Op::Link(a, b) => {
            let ino = m.lookup(r, a).await.unwrap().unwrap().ino;
            m.link(ino, r, b, Link::New, NOW).await.unwrap();
        }
        Op::Rename(a, b) => drop(m.rename((r, a), (r, b), Rename::Replace, NOW).await.unwrap()),
        Op::Unlink(n) => drop(m.unlink(r, n, false, NOW).await.unwrap()),
        Op::Rmdir(n) => drop(m.unlink(r, n, true, NOW).await.unwrap()),
        Op::Unnamed => drop(m.create_unnamed(r, OWNER, NOW).await.unwrap()),
    }
}

/// The judge of one crash state: it mounts read-write, replaying; its root
/// holds the names of some prefix of the operations, no shorter than the
/// committed one; and unmounted clean, its orphans released, it checks
/// clean.
fn judge(image: &[u8], states: &[BTreeSet<Vec<u8>>], applied: usize, committed: usize, passphrase: Option<&[u8]>) -> core::result::Result<(), String> {
    let dev = Overlay::new(image);
    let mount = match passphrase {
        Some(p) => block_on_ready(Mount::unlock(&dev, p, MIN_LIMITS, MEMORY, random())),
        None => block_on_ready(Mount::open(&dev, MEMORY, random())),
    }
    .map_err(|e| format!("mount: {e:?}"))?;
    let (entries, more) = block_on_ready(mount.readdir(mount.root(), 0, 10_000)).map_err(|e| format!("readdir: {e:?}"))?;
    assert!(!more);
    let seen: BTreeSet<Vec<u8>> = entries.into_iter().map(|e| e.name).collect();
    if !(committed..=applied).any(|k| states[k] == seen) {
        return Err(format!("{} names visible: no prefix from the {committed} committed to the {applied} applied", seen.len()));
    }
    block_on_ready(mount.unmount()).map_err(|e| format!("unmount: {e:?}"))?;
    let v = match passphrase {
        Some(p) => block_on_ready(Volume::unlock(&dev, p, MIN_LIMITS)),
        None => block_on_ready(Volume::open(&dev)),
    }
    .map_err(|e| format!("reopen: {e:?}"))?;
    let found = block_on_ready(fsck(&v)).map_err(|e| format!("fsck: {e:?}"))?;
    if !found.is_empty() {
        return Err(format!("fsck: {found:?}"));
    }
    Ok(())
}

/// `ops` in the root directory, a commit after every `every`, with every
/// crash state sampled `samples` times after each request.
fn ops_survive_a_crash_anywhere(encryption: Option<Encryption>, ops: &[Op], every: usize, samples: u64) {
    let passphrase = encryption.as_ref().map(|e| e.passphrase.to_vec());
    ops_survive_a_crash_on(formatted(64 << 20, encryption).into_inner(), passphrase.as_deref(), MEMORY, ops, every, samples);
}

/// [`ops_survive_a_crash_anywhere`] on `image`, mounted with `options`; how
/// many buckets the mount took again after an emptying.
fn ops_survive_a_crash_on(image: Vec<u8>, passphrase: Option<&[u8]>, options: MountOptions, ops: &[Op], every: usize, samples: u64) -> usize {
    let passphrase = passphrase.map(<[u8]>::to_vec);
    // The names the volume holds already stay through every state.
    let initial: BTreeSet<Vec<u8>> = {
        let dev = Bytes::new(image.clone());
        let v = match &passphrase {
            Some(p) => block_on_ready(Volume::unlock(&dev, p, MIN_LIMITS)),
            None => block_on_ready(Volume::open(&dev)),
        }
        .unwrap();
        block_on_ready(v.readdir(v.root(), 0, 10_000)).unwrap().0.into_iter().map(|e| e.name).collect()
    };
    let model = Model::new(image, 512);
    let states: Vec<BTreeSet<Vec<u8>>> = names_after(ops).into_iter().map(|s| s.union(&initial).cloned().collect()).collect();
    // Operations begun, so possibly applied, and committed.
    let applied = Cell::new(0);
    let committed = Cell::new(0);
    let judged = Cell::new(0u64);
    let seed = Cell::new(0xc4a5_4000u64);
    let dev = Watched {
        model: &model,
        after: Box::new(|m: &Model| {
            seed.set(seed.get() + 1);
            m.check(samples, seed.get(), |image| {
                judged.set(judged.get() + 1);
                judge(image, &states, applied.get(), committed.get(), passphrase.as_deref())
            })
            .unwrap_or_else(|v| panic!("{v}"));
        }),
    };
    let mount = match &passphrase {
        Some(p) => block_on_ready(Mount::unlock(&dev, p, MIN_LIMITS, options, random())),
        None => block_on_ready(Mount::open(&dev, options, random())),
    }
    .unwrap();
    Executor::new().block_on(
        async {
            for (i, op) in ops.iter().enumerate() {
                applied.set(i + 1);
                apply(&mount, op).await;
                if (i + 1) % every == 0 {
                    mount.commit().await.unwrap();
                    committed.set(i + 1);
                }
            }
        },
        || panic!("stalled"),
    );
    let reused = mount.reused();
    block_on_ready(mount.unmount()).unwrap();
    assert!(judged.get() > 500, "{} crash states judged", judged.get());
    reused
}

fn creates(n: usize) -> Vec<Op> {
    (0..n).map(|i| Op::Create(format!("c{i}").into_bytes())).collect()
}

#[test]
fn creates_survive_a_crash_at_every_request() {
    ops_survive_a_crash_anywhere(None, &creates(60), 5, 32);
}

#[test]
fn encrypted_creates_survive_a_crash_at_every_request() {
    let secret = hex_secret();
    ops_survive_a_crash_anywhere(Some(Encryption { passphrase: &secret, kdf: MIN_KDF }), &creates(30), 3, 32);
}

/// More creates than the journal holds: reclaim writes nodes and journals
/// their pointer updates between them, and a crash lands among those too.
#[test]
fn creates_through_reclaim_survive_a_crash_at_every_request() {
    ops_survive_a_crash_anywhere(None, &creates(240), 40, 16);
}

/// Every namespace operation, a crash at every request: links, renames over
/// files, unlinks leaving orphans, removed directories, unnamed files.
#[test]
fn namespace_operations_survive_a_crash_at_every_request() {
    ops_survive_a_crash_anywhere(None, &mixed(140), 10, 16);
}

#[test]
fn encrypted_namespace_operations_survive_a_crash_at_every_request() {
    let secret = hex_secret();
    ops_survive_a_crash_anywhere(Some(Encryption { passphrase: &secret, kdf: MIN_KDF }), &mixed(70), 7, 24);
}


/// A cache whose dirty-node limit is one transaction's footprint: reclaim
/// writes before every operation, so leaves fill and rewrite.
pub(super) const PRESSED: MountOptions = MountOptions { cache_nodes: 54 };

/// `files` files made, then each renamed back and forth `rounds` times:
/// the same leaves rewritten again and again, each rewrite emptying the
/// bucket of the node it replaces.
fn renames(files: usize, rounds: usize) -> Vec<Op> {
    let mut ops: Vec<Op> = (0..files).map(|i| Op::Create(format!("a{i}").into_bytes())).collect();
    for round in 0..rounds {
        let (from, to) = if round % 2 == 0 { ("a", "b") } else { ("b", "a") };
        ops.extend((0..files).map(|i| Op::Rename(format!("{from}{i}").into_bytes(), format!("{to}{i}").into_bytes())));
    }
    ops
}

/// Allocation pressure: a volume a file has filled to a few hundred empty
/// buckets, its leaves rewritten until emptied buckets are taken again, a
/// crash at every request. Every crash state replays to a prefix holding
/// every commit and checks clean: a bucket is written again only once its
/// emptying is durable. Red when one is reused before.
#[test]
fn renames_reusing_buckets_on_a_nearly_full_volume_survive_a_crash_at_every_request() {
    let mut data = Vec::new();
    let image = super::buckets::nearly_full(&mut data, 400).into_inner();
    let reused = ops_survive_a_crash_on(image, None, PRESSED, &renames(8, 8), 4, 8);
    assert!(reused > 4, "{reused} buckets taken again");
}

/// The same on DATA's encrypted form, on a volume with no data.
#[test]
fn encrypted_renames_reusing_buckets_survive_a_crash_at_every_request() {
    let secret = hex_secret();
    let image = formatted(64 << 20, Some(Encryption { passphrase: &secret, kdf: MIN_KDF })).into_inner();
    let reused = ops_survive_a_crash_on(image, Some(&secret), PRESSED, &renames(8, 6), 4, 6);
    assert!(reused > 4, "{reused} buckets taken again");
}

/// The window upstream's reader takes (A2-torn-newest A2.T1, A2.13): every
/// flush entry's header last sequence, its checksum good or not, drops each
/// entry below it as the journal is read; then, from the highest down, no-flush
/// entries are skipped, the first bad flush entry is skipped as torn, and the
/// next flush entry is `F`. `None` when none is left, which upstream cannot
/// recover from.
fn upstream_f(image: &[u8], passphrase: Option<&[u8]>) -> core::result::Result<Option<u64>, String> {
    let dev = ReadOnlyBytes::new(image).unwrap();
    let sb = block_on_ready(super::super::read::read_superblock(&dev)).map_err(|e| format!("superblock: {e:?}"))?;
    let key = match passphrase {
        Some(p) => Some(super::super::crypt::MasterKey::unwrap(sb.crypt.as_ref().ok_or("no crypt field")?, &sb.header.internal_uuid, p, MIN_LIMITS).map_err(|e| format!("unlock: {e:?}"))?),
        None => None,
    };
    let scan = block_on_ready(super::super::journal::scan(&dev, &sb, key.as_ref())).map_err(|e| format!("scan: {e:?}"))?;
    let floor = scan.entries.values().filter(|e| e.flush).map(|e| e.last_seq).max().unwrap_or(0);
    let mut torn = false;
    for e in scan.entries.values().rev().filter(|e| e.seq >= floor && e.flush) {
        if !e.good && !torn {
            torn = true;
            continue;
        }
        // A second bad flush entry ends the walk with no `F` (A2.13 step 5).
        return Ok(e.good.then_some(e.seq));
    }
    Ok(None)
}

/// `run` against a model of `image`, every crash state after each request
/// judged by upstream's window: an unclean one always leaves `F`.
fn upstream_finds_f_at_every_request(image: Vec<u8>, passphrase: Option<&[u8]>, run: impl AsyncFnOnce(&Mount<&Watched>)) -> u64 {
    let model = Model::new(image, 512);
    let unclean = Cell::new(0u64);
    let seed = Cell::new(0xa224_0000u64);
    let dev = Watched {
        model: &model,
        after: Box::new(|m: &Model| {
            seed.set(seed.get() + 1);
            m.check(64, seed.get(), |image| {
                let dev = ReadOnlyBytes::new(image).unwrap();
                let clean = block_on_ready(super::super::read::read_superblock(&dev)).map_err(|e| format!("{e:?}"))?.clean_flag();
                if clean {
                    return Ok(());
                }
                unclean.set(unclean.get() + 1);
                match upstream_f(image, passphrase)? {
                    Some(_) => Ok(()),
                    None => {
                        let dev = ReadOnlyBytes::new(image).unwrap();
                        let sb = block_on_ready(super::super::read::read_superblock(&dev)).unwrap();
                        let scan = block_on_ready(super::super::journal::scan(&dev, &sb, None)).ok();
                        let entries: Vec<_> = scan.iter().flat_map(|s| s.entries.values()).map(|e| (e.seq, e.last_seq, e.flush, e.good)).collect();
                        Err(format!("unclean, and upstream's reader finds no entry to replay: {entries:?}"))
                    }
                }
            })
            .unwrap_or_else(|v| panic!("{v}"));
        }),
    };
    let mount = match passphrase {
        Some(p) => block_on_ready(Mount::unlock(&dev, p, MIN_LIMITS, PRESSED, random())),
        None => block_on_ready(Mount::open(&dev, PRESSED, random())),
    }
    .unwrap();
    Executor::new().block_on(run(&mount), || panic!("stalled"));
    block_on_ready(mount.unmount()).unwrap();
    unclean.get()
}

/// A2.24: no flush entry names a sequence past the newest one durable when
/// it was written, so a torn newest entry leaves upstream the one before it.
/// Renames whose reclaim writes before each, a commit after every two, a
/// crash at every request: in every unclean state upstream's reader finds
/// an entry to replay. And on the medium no entry names itself but the first
/// after mkfs's empty journal (A2.25), so only that one and the unmount's
/// last pay for being written header last. Red when an entry's last
/// sequence may be its own.
#[test]
fn a_torn_newest_entry_leaves_upstream_an_entry_to_replay() {
    use super::super::read::Source;
    for passphrase in [None, Some(hex_secret())] {
        let p = passphrase.as_deref();
        let image = formatted(64 << 20, p.map(|passphrase| Encryption { passphrase, kdf: MIN_KDF })).into_inner();
        let ops = renames(6, 4);
        let unclean = upstream_finds_f_at_every_request(image, p, async |m| {
            for (i, op) in ops.iter().enumerate() {
                apply(m, op).await;
                if i % 2 == 1 {
                    m.commit().await.unwrap();
                }
            }
            let scan = super::super::journal::scan(Source::dev(m), Source::superblock(m), Source::master_key(m)).await.unwrap();
            let named: Vec<u64> = scan.entries.values().filter(|e| e.good && e.flush && e.last_seq == e.seq && e.seq != 1).map(|e| e.seq).collect();
            assert!(scan.entries.len() > 10 && named.is_empty(), "entries naming themselves: {named:?}");
        });
        assert!(unclean > 200, "{unclean} unclean states judged");
    }
}

/// A2.25: the superblock goes unclean only once a flush entry is durable,
/// on a volume whose journal is empty (mkfs's) and on one holding the entry
/// its `clean` names. Crashed at every request of going read-write and the
/// first commit, every unclean state leaves upstream an entry to replay. Red
/// when the superblock goes unclean before the entry `clean` names is
/// written.
#[test]
fn the_superblock_goes_unclean_only_once_an_entry_is_durable() {
    let fresh = formatted(64 << 20, None);
    let reopened = formatted(64 << 20, None);
    block_on_ready(block_on_ready(Mount::open(&reopened, MEMORY, random())).unwrap().unmount()).unwrap();
    for image in [fresh.into_inner(), reopened.into_inner()] {
        let unclean = upstream_finds_f_at_every_request(image, None, async |m| {
            m.create(m.root(), b"f", New::File, OWNER, NOW).await.unwrap();
            m.commit().await.unwrap();
        });
        assert!(unclean > 0, "no unclean state judged");
    }
}
