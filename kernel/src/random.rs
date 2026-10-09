//! The kernel's one source of random bytes, on every architecture:
//! `toyos_random`'s generator, keyed once at boot and drawn from by the hash
//! seed and by `SYS_RANDOM`.
//!
//! **The key** is every source this machine has, mixed: the seed the loader
//! read from firmware, and each of the architecture's CPU sources
//! ([`Source`]). A source that is absent, gave no data or gave a constant is
//! said by name and not mixed; a machine where nothing is mixed is refused by
//! name. The loader's seed is trusted for being secret and unpredictable, as
//! firmware and a hypervisor are already trusted with the whole machine, and a
//! CPU source is mixed into it, never in its place. Nothing reseeds: the key
//! descends from boot alone.
//!
//! **A draw** takes [`GENERATOR`] for one ChaCha20 block, which replaces the
//! key and keys the draw's own stream, and expands that stream with the lock
//! given back, on the calling thread. So no two draws share a key, a caller
//! holds no state a clone could duplicate, what one process drew says nothing
//! of another's bytes, and the generator's memory read later gives no earlier
//! draw.

use toyos_abi::boot::KernelArgs;
use toyos_random::{wipe, Generator, Refusal, Seed, Stream, SEED_LEN};

use crate::arch::entropy;
use crate::sync::Lock;
use crate::user_ptr::UserBytesMut;

const _: () = assert!(toyos_abi::boot::SEED_LEN == SEED_LEN);

/// A random source of the CPU's own.
pub struct Source {
    pub name: &'static str,
    /// Whether this CPU has it, or why not.
    pub available: fn() -> Result<(), &'static str>,
    /// One drawn word, or `None` where it had none to give; never waits.
    pub draw: fn() -> Option<u64>,
}

/// `None` until [`key`].
static GENERATOR: Lock<Option<Generator>> = Lock::new(None);

const LOADER: &str = "the loader's seed";

/// Key the generator, once, before the first draw. `loader` is the loader's
/// own arguments and `copy` the kernel's copy of them: the seed leaves both
/// here.
pub fn key(loader: &mut KernelArgs, copy: &mut KernelArgs) {
    let mut generator: Option<Generator> = None;
    let mut mixed = 0u32;
    let mut mix = |name: &str, seed: Result<Seed, Refusal>| match seed {
        Ok(seed) => {
            match &mut generator {
                Some(generator) => generator.mix(seed),
                None => generator = Some(Generator::keyed(seed)),
            }
            mixed += 1;
            log!("random: {name} is mixed into the generator's key");
        }
        Err(why) => log!("random: {name} is not mixed: {why}"),
    };

    // The loader's own arguments hold the same bytes: taken only to zero them.
    drop(Seed::take(&mut loader.loader_seed, &mut loader.loader_seed_len));
    match Seed::take(&mut copy.loader_seed, &mut copy.loader_seed_len) {
        None => log!("random: {LOADER} is not mixed: the loader handed none"),
        Some(seed) => mix(LOADER, seed),
    }

    for source in entropy::SOURCES {
        if let Err(why) = (source.available)() {
            log!("random: {} is not mixed: {why}", source.name);
            continue;
        }
        let mut bytes = [0u8; SEED_LEN];
        let drawn = bytes
            .as_chunks_mut::<8>()
            .0
            .iter_mut()
            .all(|word| (source.draw)().map(|drawn| *word = drawn.to_ne_bytes()).is_some());
        if drawn {
            mix(source.name, Seed::judge(&bytes));
        } else {
            log!("random: {} is not mixed: it had no data to give", source.name);
        }
        wipe(&mut bytes);
    }

    let Some(generator) = generator else {
        panic!(
            "random: nothing keyed the generator: the loader handed no seed and this CPU has no random \
             source this kernel draws from, so no byte it gave out would be random"
        )
    };
    log!("random: the generator is keyed from {mixed} source(s), and every random byte is its ChaCha20");
    assert!(GENERATOR.lock().replace(generator).is_none(), "random: key() ran twice in one boot");
}

/// One draw's stream. The lock is held for the one block that makes it.
fn stream() -> Stream {
    GENERATOR.lock().as_mut().expect("random: a draw before random::key()").stream()
}

/// A drawn word, for the kernel's own use.
pub fn word() -> u64 {
    let mut bytes = [0u8; 8];
    stream().fill(&mut bytes);
    u64::from_ne_bytes(bytes)
}

/// Fill a caller's window with one draw.
pub fn fill_user(out: &mut UserBytesMut) {
    let mut stream = stream();
    let mut chunk = [0u8; 256];
    let mut at = 0;
    while at < out.len() {
        let n = (out.len() - at).min(chunk.len());
        stream.fill(&mut chunk[..n]);
        out.write_at(at, &chunk[..n]);
        at += n;
    }
    wipe(&mut chunk);
}
