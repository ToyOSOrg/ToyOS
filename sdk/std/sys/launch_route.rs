//! Where std's spawn takes a child, decided before anything is sent: refused,
//! spawned directly, or asked of the launcher; and what a launch the launcher
//! did not make leaves a caller.
//!
//! **The supervisor starts a child only by a launch**, and a launch runs its
//! row's own program with no authority but its row's and the connectors
//! provided beside it. So a command asked of the supervisor that carries an
//! endowment or an extra slot, or names its image, is refused, as is one that
//! names its image and provides a connector; one that decided what its child
//! holds or runs is spawned directly; and one whose launch was not made is
//! spawned directly only where it asked nothing a launch alone gives.
//!
//! It names nothing outside `core`, so a host compiles it as it is
//! (`sdk/std/copies`).

/// What a command asked for before its spawn.
#[derive(Clone, Copy, Default)]
pub struct Asked {
    /// `under_supervisor`.
    pub supervisor: bool,
    /// An `endow`.
    pub endowed: bool,
    /// An `inherit_handle` slot beyond stdio.
    pub extra_slots: bool,
    /// An `image_from`.
    pub image_named: bool,
    /// A `provide`.
    pub provided: bool,
}

/// Where a spawn goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Spawned by this process.
    Direct,
    /// Asked of the launcher, where this process holds one.
    Launch,
    /// Refused `PermissionDenied`, for this reason.
    Refused(&'static str),
}

pub fn route(asked: Asked) -> Route {
    if asked.supervisor && (asked.endowed || asked.extra_slots) {
        return Route::Refused("the supervisor starts only a launch, which carries no endowment and no extra slot");
    }
    if asked.image_named && (asked.supervisor || asked.provided) {
        return Route::Refused("a launch runs its row's own program, so it takes no image the caller names");
    }
    match asked.endowed || asked.extra_slots || asked.image_named {
        true => Route::Direct,
        false => Route::Launch,
    }
}

/// Why a launch was not made.
pub enum Unmade<E> {
    /// This process holds no launcher; `provided` is a command that provided a
    /// connector, which only a launch carries.
    NoLauncher { provided: bool },
    /// No row declares the program.
    NotDeclared,
    /// The launch never left this process.
    NotSent,
    /// The copy of its place a launch carries could not be made, for this
    /// reason.
    PlaceNotCopied(E),
}

/// What a command whose launch was not made gets.
#[derive(Debug, PartialEq, Eq)]
pub enum Unlaunched<E> {
    /// The direct spawn.
    Direct,
    /// Refused `PermissionDenied`, for this reason.
    Refused(&'static str),
    /// Refused as the copy of its place was.
    Failed(E),
}

/// A launch not made is spawned directly only where the command asked
/// nothing a launch alone gives: not the supervisor, not a connector beside
/// its row, and not a place the launch was to carry, under which a direct
/// child would hold this process's namespace and not its row's.
pub fn unmade<E>(why: Unmade<E>, supervisor: bool) -> Unlaunched<E> {
    match why {
        Unmade::PlaceNotCopied(e) => Unlaunched::Failed(e),
        Unmade::NoLauncher { provided: true } => {
            Unlaunched::Refused("only a launch carries a provided connector, and this process holds no launcher")
        }
        Unmade::NoLauncher { provided: false } | Unmade::NotDeclared | Unmade::NotSent => match supervisor {
            true => Unlaunched::Refused("the supervisor did not launch it"),
            false => Unlaunched::Direct,
        },
    }
}

/// What a direct spawn runs, and the name it runs it under: the image the
/// command named, else its program; `argv[0]` is the program either way.
pub fn image_and_name<'a, T: ?Sized>(program: &'a T, image: Option<&'a T>) -> (&'a T, &'a T) {
    (image.unwrap_or(program), program)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUPERVISOR: Asked = Asked { supervisor: true, endowed: false, extra_slots: false, image_named: false, provided: false };

    /// What the supervisor would not start is refused here, and nothing is
    /// spawned in its place.
    #[test]
    fn the_supervisor_is_asked_only_by_a_launch_of_its_row_s_own_program() {
        for asked in [
            Asked { endowed: true, ..SUPERVISOR },
            Asked { extra_slots: true, ..SUPERVISOR },
            Asked { image_named: true, ..SUPERVISOR },
            Asked { image_named: true, provided: true, ..Asked::default() },
        ] {
            assert!(matches!(route(asked), Route::Refused(_)));
        }
        assert_eq!(route(SUPERVISOR), Route::Launch);
        for why in [Unmade::NoLauncher { provided: false }, Unmade::NotDeclared, Unmade::NotSent] {
            assert_eq!(unmade::<()>(why, true), Unlaunched::Refused("the supervisor did not launch it"));
        }
    }

    /// A command that decided what its child holds or runs is spawned
    /// directly, launcher or not; one that decided nothing is asked of the
    /// launcher, and spawned directly where it is not launched.
    #[test]
    fn a_command_that_decided_its_child_is_spawned_directly() {
        for asked in [
            Asked { endowed: true, ..Asked::default() },
            Asked { extra_slots: true, ..Asked::default() },
            Asked { image_named: true, ..Asked::default() },
        ] {
            assert_eq!(route(asked), Route::Direct);
        }
        assert_eq!(route(Asked::default()), Route::Launch);
        for why in [Unmade::NoLauncher { provided: false }, Unmade::NotDeclared, Unmade::NotSent] {
            assert_eq!(unmade::<()>(why, false), Unlaunched::Direct);
        }
    }

    /// A launch carries a copy of its place: one this process cannot copy
    /// refuses the launch as the copy was refused, and nothing is spawned
    /// directly in its place, whoever the command asked for.
    #[test]
    fn a_launch_whose_place_cannot_be_copied_is_refused_and_not_spawned_directly() {
        for supervisor in [false, true] {
            assert_eq!(unmade(Unmade::PlaceNotCopied("no DUP"), supervisor), Unlaunched::Failed("no DUP"));
        }
    }

    /// Only a launch carries a provided connector: with no launcher to carry
    /// it, the command is refused rather than spawned without it.
    #[test]
    fn a_provided_connector_with_no_launcher_is_refused() {
        for supervisor in [false, true] {
            assert!(matches!(unmade::<()>(Unmade::NoLauncher { provided: true }, supervisor), Unlaunched::Refused(_)));
        }
    }

    /// A command that names its image runs that image under its program's
    /// name: `argv[0]` is the program, never the image's path.
    #[test]
    fn a_named_image_runs_under_its_program_s_name() {
        assert_eq!(image_and_name("/bin/cat", Some("/tmp/other")), ("/tmp/other", "/bin/cat"));
        assert_eq!(image_and_name("/bin/cat", None), ("/bin/cat", "/bin/cat"));
    }
}
