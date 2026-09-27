//! Sizes the way the BackDown UI shows them (decimal units), and parsing for size arguments.

use anyhow::{Result, bail};

pub fn bytes(n: u64) -> String {
    let n = n as f64;
    match n {
        n if n >= 1e12 => format!("{:.2} TB", n / 1e12),
        n if n >= 1e9 => format!("{:.1} GB", n / 1e9),
        n if n >= 1e6 => format!("{:.0} MB", n / 1e6),
        n if n >= 1e3 => format!("{:.0} KB", n / 1e3),
        n => format!("{n} B"),
    }
}

pub fn count(n: u64) -> String {
    let n = n as f64;
    match n {
        n if n >= 1e6 => format!("{:.2} M", n / 1e6),
        n if n >= 1e3 => format!("{:.1} K", n / 1e3),
        n => format!("{n}"),
    }
}

/// "1M", "500K", "2G", "1.5T" or plain bytes -> bytes (decimal units, like `bytes`).
pub fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim();
    let (num, mul) = match s.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
        Some((i, _)) => {
            let mul = match s[i..].to_ascii_uppercase().trim_end_matches('B') {
                "" => 1.0,
                "K" => 1e3,
                "M" => 1e6,
                "G" => 1e9,
                "T" => 1e12,
                unit => bail!("unknown size unit {unit:?} in {s:?} (use K, M, G or T)"),
            };
            (&s[..i], mul)
        }
        None => (s, 1.0),
    };
    let n: f64 = num
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("not a size: {s:?}"))?;
    if n < 0.0 {
        bail!("size can't be negative: {s:?}");
    }
    Ok((n * mul) as u64)
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(4_200_000_000), "4.2 GB");
        assert_eq!(bytes(5_900_000_000_000), "5.90 TB");
        assert_eq!(count(26_900_000), "26.90 M");
    }

    #[test]
    fn parses() {
        assert_eq!(parse_size("1M").unwrap(), 1_000_000);
        assert_eq!(parse_size("1.5 GB").unwrap(), 1_500_000_000);
        assert_eq!(parse_size("512").unwrap(), 512);
        assert!(parse_size("3X").is_err());
    }
}
