//! Which run a registered test belongs to.
//!
//! **The table is the registration.** Every row of `tests/toyos.rs`'s
//! `MACHINE_TESTS`, `SCREEN_TESTS` and `AUDIO_TESTS` carries its [`Tier`] or
//! does not compile, and the shared boot's discovered tests share one. Moving a
//! test between tiers is editing that one word.
//!
//! The fast tier is what every plain `cargo test` runs; the nightly tier is
//! `--nightly`, run by `.github/workflows/nightly.yml`. No pull request boots a
//! guest. The line between the two is 10 s on CI's hosted shards, and nothing
//! enforces it: a name that is Nightly because its verdict is anchored to real
//! time, or because it shares a boot with one that is slow, stays where it is
//! whatever it measures.
//!
//! The local tier is the third, and the only one CI never runs: its guests are
//! of an architecture no hosted runner has been measured to boot.

/// Which run a registered test belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    /// Every `cargo test`.
    Fast,
    /// `cargo test --test toyos-build -- --nightly`.
    Nightly,
    /// Every `cargo test` on a developer's machine and no sharded run: a guest
    /// of an architecture no CI runner boots yet. The AArch64 port's stage 8
    /// (`issues/kernel/toyos-runs-on-arm64.md`) measures the runners and
    /// moves these rows to `Fast` or `Nightly`.
    Local,
}

impl Tier {
    /// Whether a run selects this tier: `nightly` is `--nightly`, `sharded`
    /// a `--shard`, which only CI's jobs pass.
    pub fn selected(self, nightly: bool, sharded: bool) -> bool {
        match self {
            Self::Fast => true,
            Self::Nightly => nightly,
            Self::Local => !sharded,
        }
    }
}
