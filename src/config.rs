//! Command line and environment configuration.

use std::path::PathBuf;
use std::time::Duration;

use clap::builder::BoolishValueParser;
use clap::{ArgAction, Parser};
use regex::Regex;

/// Evict least recently used podman images to keep image storage under a byte budget.
#[derive(Debug, Parser)]
#[command(name = "podman-lru", version, about)]
pub struct Config {
    /// Byte budget for all images, e.g. 50GB, 800MiB, 1T (decimal or binary units).
    #[arg(long, env = "PODMAN_LRU_MAX_SIZE", value_parser = parse_size)]
    pub max_size: u64,

    /// libpod API unix socket.
    #[arg(
        long,
        env = "PODMAN_LRU_SOCKET",
        default_value = "/run/podman/podman.sock"
    )]
    pub socket: PathBuf,

    /// Where last-used timestamps are persisted.
    #[arg(long, env = "PODMAN_LRU_STATE", default_value = "/state/state.json")]
    pub state: PathBuf,

    /// Period of the reconcile and enforce safety net (e.g. 60s, 5m).
    #[arg(long, env = "PODMAN_LRU_INTERVAL", default_value = "60s", value_parser = humantime::parse_duration)]
    pub interval: Duration,

    /// Regex; images with any name matching it are never evicted.
    #[arg(long, env = "PODMAN_LRU_KEEP")]
    pub keep: Option<Regex>,

    /// Log what would be evicted without deleting anything.
    #[arg(long, env = "PODMAN_LRU_DRY_RUN", default_value = "false", action = ArgAction::Set, value_parser = BoolishValueParser::new(), num_args = 0..=1, default_missing_value = "true")]
    pub dry_run: bool,
}

/// Parse sizes like `50GB`, `800 MiB`, `1T`, `1024`. Decimal units (KB, MB, GB, TB)
/// are powers of 1000, binary ones (KiB, MiB, GiB, TiB) powers of 1024, and a bare
/// letter (K, M, G, T) is binary.
pub fn parse_size(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number
        .parse()
        .map_err(|_| format!("invalid size `{text}`: expected a number followed by a unit"))?;
    let multiplier: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kib" => 1024.0,
        "m" | "mib" => 1024f64.powi(2),
        "g" | "gib" => 1024f64.powi(3),
        "t" | "tib" => 1024f64.powi(4),
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        other => return Err(format!("invalid size `{text}`: unknown unit `{other}`")),
    };
    let bytes = number * multiplier;
    if !bytes.is_finite() || bytes < 0.0 {
        return Err(format!("invalid size `{text}`"));
    }
    Ok(bytes.round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("50GB").unwrap(), 50_000_000_000);
        assert_eq!(parse_size("800 MiB").unwrap(), 800 * 1024 * 1024);
        assert_eq!(parse_size("1T").unwrap(), 1 << 40);
        assert_eq!(parse_size("1.5kb").unwrap(), 1500);
        assert!(parse_size("abc").is_err());
        assert!(parse_size("1 parsec").is_err());
    }

    #[test]
    fn cli_parses_env_style_values() {
        let cfg = Config::try_parse_from([
            "podman-lru",
            "--max-size",
            "10G",
            "--dry-run",
            "yes",
            "--keep",
            "pandoc|^localhost/tools/",
            "--interval",
            "5m",
        ])
        .unwrap();
        assert_eq!(cfg.max_size, 10 << 30);
        assert!(cfg.dry_run);
        assert!(cfg.keep.unwrap().is_match("localhost/tools/x:1"));
        assert_eq!(cfg.interval, Duration::from_secs(300));
        let cfg = Config::try_parse_from(["podman-lru", "--max-size", "1G", "--dry-run"]).unwrap();
        assert!(cfg.dry_run);
    }
}
