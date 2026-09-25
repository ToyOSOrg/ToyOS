//! Which partition is ROOT: the one candidate whose filesystem is the one the
//! boot parameter names. None, or more than one, is a boot refused by name,
//! and so is a table carrying more candidates than the loader looked at,
//! because a match among the ones it did not look at would go unseen.

/// Why the boot disk names no one ROOT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// The table carries `matched` candidates and the loader looked at `listed`.
    Unlisted { matched: u32, listed: usize },
    /// `matches` of the `candidates` carry the filesystem named, and only one is an answer.
    Matches { matches: usize, candidates: usize },
}

/// The one candidate whose filesystem is `named`.
///
/// `candidates` are every candidate the loader looked at, each with the name
/// its filesystem carries, `None` for one that carries none it can read;
/// `matched` is how many the table carries.
pub fn pick<P: Copy, U: PartialEq>(named: &U, candidates: &[(P, Option<U>)], matched: u32) -> Result<P, Refused> {
    if usize::try_from(matched).map_or(true, |matched| matched > candidates.len()) {
        return Err(Refused::Unlisted { matched, listed: candidates.len() });
    }
    let named_by = |(_, uuid): &&(P, Option<U>)| uuid.as_ref() == Some(named);
    let matches = candidates.iter().filter(named_by).count();
    match candidates.iter().find(named_by) {
        Some(&(one, _)) if matches == 1 => Ok(one),
        _ => Err(Refused::Matches { matches, candidates: candidates.len() }),
    }
}
