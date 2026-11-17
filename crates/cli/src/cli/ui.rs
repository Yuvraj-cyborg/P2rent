//! Progress bars and human-readable formatting.

use indicatif::{HumanBytes, ProgressBar, ProgressStyle};
use mux::Protocol;
use std::time::Duration;

const TICK: Duration = Duration::from_millis(120);

pub(super) fn spinner(message: impl Into<String>) -> ProgressBar {
    let bar = ProgressBar::new_spinner();
    bar.set_style(style("{spinner} {msg}"));
    bar.set_message(message.into());
    bar.enable_steady_tick(TICK);
    bar
}

/// A byte-count bar with throughput and time remaining.
pub(super) fn bytes_bar(total: u64, label: &str) -> ProgressBar {
    let bar = ProgressBar::new(total);
    bar.set_style(
        style(
            "{prefix:>11} [{bar:32}] {bytes:>10}/{total_bytes:<10} {binary_bytes_per_sec:>12}  eta {eta:<5} {msg}",
        )
        .progress_chars("=> "),
    );
    bar.set_prefix(label.to_owned());
    bar
}

fn style(template: &str) -> ProgressStyle {
    ProgressStyle::with_template(template).expect("progress templates are static and valid")
}

pub(super) fn bytes(n: u64) -> String {
    HumanBytes(n).to_string()
}

pub(super) fn rate(bytes: u64, elapsed: Duration) -> String {
    let millis = elapsed.as_millis();
    if millis == 0 {
        return "-".to_owned();
    }
    let per_second = u128::from(bytes) * 1000 / millis;
    format!(
        "{}/s",
        HumanBytes(u64::try_from(per_second).unwrap_or(u64::MAX))
    )
}

pub(super) fn plural(n: impl TryInto<u64>, noun: &str) -> String {
    match n.try_into().unwrap_or(u64::MAX) {
        1 => format!("1 {noun}"),
        n => format!("{n} {noun}s"),
    }
}

/// One line for each protocol, under a heading: its scheme, whether it can
/// be read and written, and what it is.
pub(super) fn protocols(protocols: &[Protocol]) -> String {
    let yes_no = |able| if able { "yes" } else { "no" };
    let rows = protocols.iter().map(|protocol| {
        format!(
            "{:<6}  {:<4}  {:<5}  {}\n",
            protocol.scheme,
            yes_no(protocol.reads),
            yes_no(protocol.writes),
            protocol.about,
        )
    });
    std::iter::once("SCHEME  READ  WRITE  PLACE\n".to_owned())
        .chain(rows)
        .collect()
}

/// Parses sizes such as `1048576`, `512K`, `4M`, `1MiB` or `16MB`. Suffixes
/// are binary: K is 1024.
pub(super) fn parse_size(text: &str) -> Result<u32, String> {
    let text = text.trim();
    let digits = text.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let multiplier: u64 = match text[digits.len()..].to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        other => return Err(format!("unknown size suffix {other:?}")),
    };
    digits
        .trim()
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| format!("{text:?} is not a valid size"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("4096"), Ok(4096));
        assert_eq!(parse_size("512K"), Ok(512 * 1024));
        assert_eq!(parse_size("1MiB"), Ok(1024 * 1024));
        assert_eq!(parse_size("4 mb"), Ok(4 * 1024 * 1024));
        assert!(parse_size("").is_err());
        assert!(parse_size("1X").is_err());
        assert!(parse_size("8G").is_err());
    }

    #[test]
    fn pluralises() {
        assert_eq!(plural(1, "peer"), "1 peer");
        assert_eq!(plural(3, "peer"), "3 peers");
    }
}
