//! What the timed benchmarks share: settings read from the environment,
//! the CPU time this process has used, and the report of a set of runs.

use std::time::Duration;

pub(crate) const MIB: u64 = 1024 * 1024;

/// A whole number from the environment, or `default` if it is not set.
pub(crate) fn setting(name: &str, default: u64) -> u64 {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .unwrap_or_else(|_| panic!("{name} must be a whole number, got {value:?}")),
        Err(_) => default,
    }
}

pub(crate) fn rate(bytes: u64, elapsed: Duration) -> String {
    format!(
        "{:>5.0} MiB/s  ({:.2}s)",
        bytes as f64 / MIB as f64 / elapsed.as_secs_f64(),
        elapsed.as_secs_f64()
    )
}

/// The runs of one benchmark, each of which moves `size` bytes.
pub(crate) struct Runs {
    size: u64,
    rates: Vec<f64>,
    cpu_per_gib: Vec<f64>,
}

impl Runs {
    pub(crate) fn new(size: u64) -> Self {
        Self {
            size,
            rates: Vec::new(),
            cpu_per_gib: Vec::new(),
        }
    }

    /// Prints one run, which took `elapsed` and began when this process
    /// had used `cpu_before`.
    pub(crate) fn record(
        &mut self,
        run: u64,
        elapsed: Duration,
        cpu_before: Option<(Duration, Duration)>,
    ) {
        let gib = self.size as f64 / (1024.0 * MIB as f64);
        self.rates
            .push(self.size as f64 / MIB as f64 / elapsed.as_secs_f64());
        let cpu = cpu_time().zip(cpu_before).map(|(after, before)| {
            (
                after.0.saturating_sub(before.0),
                after.1.saturating_sub(before.1),
            )
        });
        match cpu {
            Some((user, system)) => {
                let (user, system) = (user.as_secs_f64() / gib, system.as_secs_f64() / gib);
                self.cpu_per_gib.push(user + system);
                println!(
                    "  run {run}   {}   {:.2} cpu-s/GiB  (user {user:.2}, system {system:.2})",
                    rate(self.size, elapsed),
                    user + system,
                );
            }
            None => println!("  run {run}   {}", rate(self.size, elapsed)),
        }
    }

    /// Prints the run in the middle. CPU time is steadier than wall time,
    /// which also depends on the disk.
    pub(crate) fn report(mut self) {
        self.rates.sort_by(f64::total_cmp);
        self.cpu_per_gib.sort_by(f64::total_cmp);
        let rate = self.rates[self.rates.len() / 2];
        match self.cpu_per_gib.get(self.cpu_per_gib.len() / 2) {
            Some(cost) => println!("median    {rate:>5.0} MiB/s            {cost:.2} cpu-s/GiB"),
            None => println!("median    {rate:>5.0} MiB/s"),
        }
    }
}

/// User and system CPU time used by this process so far.
#[cfg(unix)]
pub(crate) fn cpu_time() -> Option<(Duration, Duration)> {
    use nix::sys::resource::{UsageWho, getrusage};
    use nix::sys::time::TimeValLike;

    let usage = getrusage(UsageWho::RUSAGE_SELF).ok()?;
    let duration = |micros: i64| u64::try_from(micros).ok().map(Duration::from_micros);
    Some((
        duration(usage.user_time().num_microseconds())?,
        duration(usage.system_time().num_microseconds())?,
    ))
}

#[cfg(not(unix))]
pub(crate) fn cpu_time() -> Option<(Duration, Duration)> {
    None
}
