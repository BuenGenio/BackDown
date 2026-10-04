//! `backdown license add|show`: the BackDown Pro licence key, checked offline.
//!
//! A key is `BD1.<base64url JSON claims>.<base64url Ed25519 signature over "BD1.<claims>">`, issued by
//! backdown.io when an order is paid (marketing/src/license.js). Only the matching private key can mint one, so
//! checking it needs no network and no account. The key is kept in the per-user data folder.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::locations;

/// backdown.io's licence-signing public key
const PUBLIC_KEY: [u8; 32] =
    hex32("16ac6f9b2da14811a852880437b302766f9b6b943e92a33b86ba645eb49992c1");
const FILE: &str = "license.txt";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Licence {
    pub v: u32,
    pub id: String,
    pub product: String,
    pub kind: String,
    pub seats: u32,
    pub email: String,
    #[serde(default)]
    pub name: Option<String>,
    pub issued: String,
    /// last day it unlocks anything (YYYY-MM-DD); none on keys that don't lapse, or haven't started
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
}

/// 64 lowercase hex digits as 32 bytes, at compile time (the built-in public keys).
pub(crate) const fn hex32(s: &str) -> [u8; 32] {
    let b = s.as_bytes();
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = (nibble(b[2 * i]) << 4) | nibble(b[2 * i + 1]);
        i += 1;
    }
    out
}

const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => panic!("not lowercase hex"),
    }
}

/// Check a key's signature and read its claims.
pub fn parse(key: &str, public_key: &[u8; 32]) -> Result<Licence> {
    let key = key.trim();
    let mut parts = key.split('.');
    let (Some("BD1"), Some(claims), Some(sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        bail!("that isn't a BackDown licence key (it should start with BD1. and have three parts)");
    };
    let sig = URL_SAFE_NO_PAD
        .decode(sig)
        .ok()
        .and_then(|s| Signature::from_slice(&s).ok())
        .context("the key's signature is damaged; copy the whole key again")?;
    let vk = VerifyingKey::from_bytes(public_key).expect("valid built-in public key");
    vk.verify_strict(format!("BD1.{claims}").as_bytes(), &sig)
        .map_err(|_| {
            anyhow::anyhow!(
                "this key wasn't issued by backdown.io, or it was changed after it was issued"
            )
        })?;
    let json = URL_SAFE_NO_PAD
        .decode(claims)
        .context("the key is damaged")?;
    let licence: Licence = serde_json::from_slice(&json)
        .context("the key is from a newer BackDown; update BackDown")?;
    if licence.v != 1 {
        bail!(
            "this key is for a newer BackDown (format {}); update BackDown",
            licence.v
        );
    }
    Ok(licence)
}

pub fn path() -> PathBuf {
    locations::state_dir().join(FILE)
}

/// The licence kept in the data folder: none, or a valid one (an invalid file is an error).
pub fn load() -> Result<Option<Licence>> {
    let p = path();
    match std::fs::read_to_string(&p) {
        Ok(k) => parse(&k, &PUBLIC_KEY)
            .map(Some)
            .with_context(|| format!("the licence in {} isn't valid", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("can't read {}", p.display())),
    }
}

/// Check a key and keep it (replacing any earlier one); what `license add` and the Settings view do.
pub fn store(key: &str) -> Result<Licence> {
    let key = key.trim();
    let licence = parse(key, &PUBLIC_KEY)?;
    let p = path();
    if let Some(dir) = p.parent() {
        locations::ensure_private_dir(dir, false)
            .with_context(|| format!("can't create {}", dir.display()))?;
    }
    std::fs::write(&p, format!("{key}\n"))
        .with_context(|| format!("can't write {}", p.display()))?;
    Ok(licence)
}

/// What a licence's seats count: sites for Forensic, machines otherwise.
fn seats_text(l: &Licence) -> String {
    let unit = if l.product == "forensic" {
        "site"
    } else {
        "machine"
    };
    format!("{} {unit}{}", l.seats, if l.seats == 1 { "" } else { "s" })
}

fn describe(l: &Licence) {
    let edition = titlecase(&l.product);
    println!("BackDown {edition} licence {}", l.id);
    println!(
        "  for {}{} · {} · issued {}",
        l.name
            .as_deref()
            .map(|n| format!("{n} "))
            .unwrap_or_default(),
        if l.name.is_some() {
            format!("<{}>", l.email)
        } else {
            l.email.clone()
        },
        seats_text(l),
        l.issued
    );
    if let Some(e) = &l.expires {
        println!("  unlocks {edition} features until {e}");
    }
    if l.kind == "preorder" {
        println!(
            "  pre-order: {edition} features switch on in the release that ships them; your first year starts then"
        );
    }
}

fn titlecase(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// `backdown license add KEY` (or `-` to read it from stdin)
pub fn add(key: &str) -> Result<ExitCode> {
    let key = if key == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
        s
    } else {
        key.to_string()
    };
    let licence = store(&key)?;
    describe(&licence);
    println!("  {}", crate::tiers::access().summary());
    eprintln!("saved to {}", path().display());
    Ok(ExitCode::SUCCESS)
}

/// `backdown license show`: exit 0 with a valid licence, 1 without one
pub fn show() -> Result<ExitCode> {
    let p = path();
    let key = match std::fs::read_to_string(&p) {
        Ok(k) => k,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "No licence: this is BackDown Community (free). Add a Pro key with `backdown license add <key>`."
            );
            println!("{}", crate::tiers::access().summary());
            return Ok(ExitCode::FAILURE);
        }
        Err(e) => return Err(e).with_context(|| format!("can't read {}", p.display())),
    };
    let licence = parse(&key, &PUBLIC_KEY)
        .with_context(|| format!("the licence in {} isn't valid", p.display()))?;
    describe(&licence);
    println!("  {}", crate::tiers::access().summary());
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forensic_seats_are_sites() {
        let l = |product: &str, seats| Licence {
            v: 1,
            id: "X".into(),
            product: product.into(),
            kind: "order".into(),
            seats,
            email: "alice@example.com".into(),
            name: None,
            issued: "2026-10-04".into(),
            expires: None,
        };
        assert_eq!(seats_text(&l("pro", 1)), "1 machine");
        assert_eq!(seats_text(&l("team", 5)), "5 machines");
        assert_eq!(seats_text(&l("forensic", 1)), "1 site");
        assert_eq!(seats_text(&l("forensic", 3)), "3 sites");
    }

    // signed with the development key (not the production one), whose public half is:
    const DEV: [u8; 32] = hex32("6dbda5c47559c8708d44f1d00acc3d9aa5d705fa6894881e7f3337bfb979c387");
    const KEY: &str = "BD1.eyJ2IjoxLCJpZCI6IlRFU1QwLUFBQUFBLUJCQkJCLUNDQ0NDIiwicHJvZHVjdCI6InBybyIsImtpbmQiOiJwcmVvcmRlciIsInNlYXRzIjoyLCJlbWFpbCI6InRlc3RAZXhhbXBsZS5jb20iLCJuYW1lIjoiVGVzdCBCdXllciIsImlzc3VlZCI6IjIwMjYtMDktMjgifQ.g842mXyW9CxlKYWKkNitBklIC2xfgftPFL8dquNC2a-LvqB5Hiu6MdjrY-wcF5yULZqkxzqxTUZLfp_3Nxj6BA";

    #[test]
    fn a_signed_key_parses() {
        let l = parse(&format!("  {KEY}\n"), &DEV).unwrap();
        assert_eq!(
            (l.product.as_str(), l.kind.as_str(), l.seats),
            ("pro", "preorder", 2)
        );
        assert_eq!(l.id, "TEST0-AAAAA-BBBBB-CCCCC");
        assert_eq!(l.name.as_deref(), Some("Test Buyer"));
    }

    #[test]
    fn the_production_key_rejects_a_dev_signed_key() {
        assert!(parse(KEY, &PUBLIC_KEY).is_err());
    }

    #[test]
    fn changed_claims_fail() {
        let (_, rest) = KEY.split_once('.').unwrap();
        let (claims, sig) = rest.split_once('.').unwrap();
        let mut json: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).unwrap()).unwrap();
        json["seats"] = 50.into();
        let forged = format!("BD1.{}.{sig}", URL_SAFE_NO_PAD.encode(json.to_string()));
        assert!(
            parse(&forged, &DEV)
                .unwrap_err()
                .to_string()
                .contains("wasn't issued")
        );
    }

    #[test]
    fn junk_is_explained() {
        assert!(
            parse("hello", &DEV)
                .unwrap_err()
                .to_string()
                .contains("isn't a BackDown licence key")
        );
        assert!(
            parse("BD1.abc.!!!", &DEV)
                .unwrap_err()
                .to_string()
                .contains("damaged")
        );
    }
}
