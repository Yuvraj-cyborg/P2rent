//! How a command ends: the number the shell sees, and what to do next.
//!
//! The numbers are part of the tool's interface. A program that runs `mux`
//! can tell from them whether running it again is worth it.

use mux::engine;
use mux::place::Failure;
use std::process::ExitCode;

/// What became of a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Exit {
    /// It failed for a reason that none of the others covers.
    Failed = 1,
    /// The command line is wrong, or names a place that cannot be used.
    Usage = 2,
    /// Nothing is at the source.
    Absent = 3,
    /// The source could not be reached, or stopped sending. Running the
    /// command again may well work.
    Unreachable = 4,
    /// The source changed while it was being read.
    Changed = 5,
    /// What arrived is not what was asked for.
    Corrupt = 6,
}

/// A command line that is written wrong, where only the command can tell.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(super) struct Misuse(pub(super) String);

/// Ends a command that asks for what cannot be done, however often it is
/// run: the places or the flags have to be written another way.
pub(super) fn misuse<T>(why: impl Into<String>) -> anyhow::Result<T> {
    Err(Misuse(why.into()).into())
}

impl Exit {
    /// How `error` ends a command: by the first cause in it that is a
    /// failure of a known kind.
    pub(super) fn of(error: &anyhow::Error) -> Self {
        let known = error.chain().find_map(|cause| {
            if cause.is::<Misuse>() {
                return Some(Self::Usage);
            }
            if let Some(failure) = cause.downcast_ref::<Failure>() {
                return Some(Self::of_failure(failure));
            }
            if let Some(error) = cause.downcast_ref::<engine::Error>() {
                return Some(Self::of_copy(error));
            }
            if let Some(error) = cause.downcast_ref::<mux::store::Error>() {
                return Some(Self::of_scan(error));
            }
            if let Some(error) = cause.downcast_ref::<mux::sync::Error>() {
                return Some(match error {
                    mux::sync::Error::Copy(error) => Self::of_copy(error),
                    mux::sync::Error::Scan(error) => Self::of_scan(error),
                    mux::sync::Error::Stopped(_) | mux::sync::Error::Change { .. } => Self::Failed,
                });
            }
            match cause.downcast_ref::<mux::Error>()? {
                mux::Error::InvalidLocation(_) | mux::Error::InvalidTicket(_) => Some(Self::Usage),
                mux::Error::Stalled(_) => Some(Self::Unreachable),
                mux::Error::ManifestUnavailable => Some(Self::Absent),
                mux::Error::Place(failure) => Some(Self::of_failure(failure)),
                _ => None,
            }
        });
        known.unwrap_or(Self::Failed)
    }

    /// How a copy that failed ends a command.
    fn of_copy(error: &engine::Error) -> Self {
        match error {
            engine::Error::Stalled(_) => Self::Unreachable,
            engine::Error::Changed(_) => Self::Changed,
            engine::Error::Unsupplied(_) => Self::Failed,
            engine::Error::Place(failure) => Self::of_failure(failure),
        }
    }

    /// How a folder that could not be looked at ends a command.
    fn of_scan(error: &mux::store::Error) -> Self {
        match error {
            mux::store::Error::Io(e) if e.kind() == std::io::ErrorKind::NotFound => Self::Absent,
            _ => Self::Failed,
        }
    }

    fn of_failure(failure: &Failure) -> Self {
        match failure {
            Failure::Absent(_) => Self::Absent,
            Failure::Unreachable(_) => Self::Unreachable,
            Failure::Changed(_) => Self::Changed,
            Failure::Corrupt(_) => Self::Corrupt,
            Failure::Fatal(_) => Self::Failed,
        }
    }

    /// The kind in a word, as a program is told it.
    pub(super) fn kind(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Usage => "usage",
            Self::Absent => "absent",
            Self::Unreachable => "unreachable",
            Self::Changed => "changed",
            Self::Corrupt => "corrupt",
        }
    }

    /// What to do next, where there is something to say.
    pub(super) fn hint(self) -> Option<&'static str> {
        match self {
            Self::Failed | Self::Usage => None,
            Self::Absent => Some("check that the source is written as it should be"),
            Self::Unreachable => {
                Some("run the same command again: it carries on with what it has stored")
            }
            Self::Changed => Some("run the same command again to copy the source as it is now"),
            Self::Corrupt => Some(
                "the source does not hold what was asked for: check the digest, and where it came from",
            ),
        }
    }
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> Self {
        ExitCode::from(exit as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use std::time::Duration;

    #[test]
    fn a_command_ends_by_the_kind_of_its_failure() {
        let said = String::new;
        let io = |kind| mux::store::Error::Io(std::io::Error::from(kind));
        let sync = |error: mux::sync::Error| anyhow::Error::from(error);
        let cases: [(anyhow::Error, Exit); 20] = [
            (
                sync(engine::Error::Stalled(Duration::ZERO).into()),
                Exit::Unreachable,
            ),
            (
                sync(engine::Error::Place(Failure::Changed(said())).into()),
                Exit::Changed,
            ),
            (sync(io(std::io::ErrorKind::NotFound).into()), Exit::Absent),
            (sync(mux::sync::Error::Stopped(said())), Exit::Failed),
            (
                sync(mux::sync::Error::Change {
                    path: std::path::PathBuf::new(),
                    source: std::io::Error::from(std::io::ErrorKind::NotFound),
                }),
                Exit::Failed,
            ),
            (io(std::io::ErrorKind::NotFound).into(), Exit::Absent),
            (
                io(std::io::ErrorKind::PermissionDenied).into(),
                Exit::Failed,
            ),
            (Failure::Absent(said()).into(), Exit::Absent),
            (Failure::Unreachable(said()).into(), Exit::Unreachable),
            (Failure::Changed(said()).into(), Exit::Changed),
            (Failure::Corrupt(said()).into(), Exit::Corrupt),
            (Failure::Fatal(said()).into(), Exit::Failed),
            (
                engine::Error::Stalled(Duration::ZERO).into(),
                Exit::Unreachable,
            ),
            (engine::Error::Changed(said()).into(), Exit::Changed),
            (engine::Error::Unsupplied(said()).into(), Exit::Failed),
            (
                engine::Error::Place(Failure::Corrupt(said())).into(),
                Exit::Corrupt,
            ),
            (mux::Error::InvalidLocation(said()).into(), Exit::Usage),
            (Misuse(said()).into(), Exit::Usage),
            (mux::Error::ManifestUnavailable.into(), Exit::Absent),
            (anyhow::anyhow!("something else"), Exit::Failed),
        ];
        for (error, exit) in cases {
            assert_eq!(Exit::of(&error), exit, "{error:#}");
        }
    }

    /// What a failure is does not change with what is said around it.
    #[test]
    fn the_kind_is_found_under_what_was_added_to_it() {
        let failed: anyhow::Result<()> = Err(Failure::Absent(String::new()).into());
        let error = failed.context("cannot copy the file").unwrap_err();
        assert_eq!(Exit::of(&error), Exit::Absent);
    }

    #[test]
    fn the_numbers_and_the_words_do_not_change() {
        let numbers = [
            Exit::Failed,
            Exit::Usage,
            Exit::Absent,
            Exit::Unreachable,
            Exit::Changed,
            Exit::Corrupt,
        ]
        .map(|exit| (exit as u8, exit.kind()));
        let kinds = [
            (1, "failed"),
            (2, "usage"),
            (3, "absent"),
            (4, "unreachable"),
            (5, "changed"),
            (6, "corrupt"),
        ];
        assert_eq!(numbers, kinds);
    }
}
