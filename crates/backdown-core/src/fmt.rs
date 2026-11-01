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

/// Unix seconds → "31 May 2020" (UTC), without a date library.
pub fn day(t: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    // civil from days (Howard Hinnant)
    let z = t.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{d} {} {y}", MONTHS[(m - 1) as usize])
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The bytes `hex` wrote (either case): `None` for an odd length or anything but hex digits
/// (no signs or spaces, which `u8::from_str_radix` would let through).
pub fn unhex(s: impl AsRef<[u8]>) -> Option<Vec<u8>> {
    let s = s.as_ref();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    s.chunks(2)
        .map(|p| Some((digit(p[0])? << 4) | digit(p[1])?))
        .collect()
}

/// A SHA-256 or an Ed25519 key from its 64 hex digits.
pub fn unhex32(s: impl AsRef<[u8]>) -> Option<[u8; 32]> {
    unhex(s)?.try_into().ok()
}

/// The text with its first letter in capitals ("drive failed" -> "Drive failed").
pub fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_rejects_what_isnt_hex() {
        let b = [0u8, 0x0f, 0xa5, 0xff];
        assert_eq!(hex(&b), "000fa5ff");
        assert_eq!(unhex("000fa5ff").unwrap(), b);
        assert_eq!(unhex("000FA5FF").unwrap(), b);
        assert_eq!(unhex(b"00ff").unwrap(), [0, 0xff]);
        assert_eq!(unhex("").unwrap(), Vec::<u8>::new());
        assert!(unhex("abc").is_none());
        assert!(unhex("gg").is_none());
        assert!(unhex("+f").is_none());
        assert!(unhex(" f").is_none());
        assert!(unhex("é0").is_none());
        assert_eq!(unhex32([b'a'; 64]).unwrap(), [0xaa; 32]);
        assert!(unhex32("aa").is_none());
        assert!(unhex32([b'g'; 64]).is_none());
    }

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
