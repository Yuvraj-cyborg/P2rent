//! Stopping a command: at a moment of the test's choosing, as a crash or a
//! power cut would, or when the test is over.

use std::process::{Child, Command};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The most rounds a test tries for each round it wants. More than any run
/// needs: every late kill halves the time the next is drawn from.
const PATIENCE: usize = 10;

/// A command that goes on until it is stopped, and is stopped when the test
/// lets go of it, however the test ends.
pub(crate) struct KillOnDrop(pub(crate) Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Starts `command` and stops it dead `after` that long, with no chance to
/// tidy up. If it ends by itself first, this returns then: there is
/// nothing left to kill, and nothing to wait for.
pub(crate) fn kill(command: &mut Command, after: Duration) {
    let mut child = command.spawn().unwrap();
    let until = Instant::now() + after;
    loop {
        if child.try_wait().unwrap().is_some() {
            return;
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(left.min(Duration::from_millis(1)));
    }
    // It may have ended this instant.
    let _ = child.kill();
    child.wait().unwrap();
}

/// When to kill a command, round after round.
///
/// A round kills the command twice, so that one that carries on is stopped
/// too, and then lets it finish. The first kill of a round comes somewhere
/// before the command would have ended by itself, as far as that is known.
///
/// How long a command takes is measured once, and that is not to be
/// trusted: on a machine that runs other tests at the same moment, one run
/// can take many times as long as the next. Drawn from a time that is too
/// long, most kills come after the command has finished and stop nothing.
/// So each time the first kill of a round comes too late, the moments that
/// follow are drawn from half the time, and that round does not count.
pub(crate) struct Moments {
    state: u64,
    within: Duration,
    wanted: usize,
    stopped: usize,
    rounds: usize,
}

impl Moments {
    /// Moments for `rounds` rounds in which the command is stopped short,
    /// where an undisturbed run took `whole`. They are seeded from the
    /// clock, so that every run of a test tries others.
    pub(crate) fn new(rounds: usize, whole: Duration) -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let seed = now.as_secs() ^ (u64::from(now.subsec_nanos()) << 32);
        Self::seeded(rounds, whole, seed)
    }

    fn seeded(rounds: usize, whole: Duration, seed: u64) -> Self {
        Self {
            // The generator never leaves zero.
            state: seed | 1,
            within: whole,
            wanted: rounds,
            stopped: 0,
            rounds: 0,
        }
    }

    /// Begins a round, and gives its number. `None` once enough rounds
    /// stopped the command short.
    ///
    /// # Panics
    ///
    /// If round after round stops nothing: something other than timing is
    /// wrong then.
    pub(crate) fn round(&mut self) -> Option<usize> {
        if self.stopped >= self.wanted {
            return None;
        }
        assert!(
            self.rounds < self.wanted * PATIENCE,
            "{} rounds stopped the command short, of {} tried",
            self.stopped,
            self.rounds
        );
        self.rounds += 1;
        Some(self.rounds - 1)
    }

    /// How long to let the command run before the next kill.
    pub(crate) fn next(&mut self) -> Duration {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        let part = (self.state >> 11) as f64 / (1u64 << 53) as f64;
        self.within.mul_f64(part)
    }

    /// Says what the first kill of a round found: whether the command was
    /// stopped before it had finished.
    pub(crate) fn first_kill(&mut self, stopped_short: bool) {
        if stopped_short {
            self.stopped += 1;
        } else {
            self.within /= 2;
        }
    }
}

#[test]
fn a_time_that_is_far_too_long_is_found_out() {
    // A command that takes a thousandth of what it was said to.
    let (said, takes) = (Duration::from_secs(1000), Duration::from_secs(1));
    for seed in 0..50 {
        let mut moments = Moments::seeded(6, said, seed);
        let mut rounds = 0;
        while moments.round().is_some() {
            let kill = moments.next();
            moments.first_kill(kill < takes);
            rounds += 1;
        }
        // Ten halvings bring the time down to the right one.
        assert!((6..=30).contains(&rounds), "seed {seed}: {rounds} rounds");
    }
}

#[test]
fn a_command_that_never_stops_short_is_given_up_on() {
    let tried = std::panic::catch_unwind(|| {
        let mut moments = Moments::seeded(2, Duration::from_secs(1), 7);
        while moments.round().is_some() {
            moments.next();
            moments.first_kill(false);
        }
    });
    assert!(tried.is_err());
}
