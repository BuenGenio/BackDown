//! Signing what BackDown attests (drive manifests, chain-of-custody exports): each installation
//! has an Ed25519 key of its own, made on first use and kept in its data folder, readable by its
//! owner alone. A signed document says which key signed it; checking one needs only the document.
//!
//! Documents are JSON. What's signed is the document's canonical form (`canonical`: keys sorted,
//! no spaces) without its `sig`, so the same document always gives the same bytes, however it
//! was written or reformatted since.

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::fmt::{hex, unhex};

/// This installation's signing key.
pub struct Signer {
    key: SigningKey,
}

impl Signer {
    /// The key at `path` (32 bytes, hex), made there first if there's none. The file is created
    /// readable by its owner alone, and never overwritten.
    pub fn load_or_create(path: &Path) -> Result<Signer> {
        match fs::read_to_string(path) {
            Ok(s) => Self::from_hex(s.trim())
                .with_context(|| format!("{} isn't a BackDown signing key", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut seed = [0u8; 32];
                getrandom::fill(&mut seed)
                    .map_err(|e| anyhow::anyhow!("no randomness for a signing key: {e}"))?;
                if let Some(dir) = path.parent() {
                    fs::create_dir_all(dir)?;
                }
                let mut o = fs::OpenOptions::new();
                o.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    o.mode(0o600);
                }
                match o.open(path) {
                    Ok(mut f) => {
                        f.write_all(hex(&seed).as_bytes())?;
                        f.sync_all()?;
                        Ok(Signer {
                            key: SigningKey::from_bytes(&seed),
                        })
                    }
                    // made by another process meanwhile: use that one
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        Self::load_or_create(path)
                    }
                    Err(e) => Err(e).with_context(|| format!("can't create {}", path.display())),
                }
            }
            Err(e) => Err(e).with_context(|| format!("can't read {}", path.display())),
        }
    }

    fn from_hex(s: &str) -> Result<Signer> {
        let b = unhex(s).context("not hex")?;
        let seed: [u8; 32] = b.try_into().map_err(|_| anyhow::anyhow!("not 32 bytes"))?;
        Ok(Signer {
            key: SigningKey::from_bytes(&seed),
        })
    }

    /// The public key, hex: what a signed document names, and what checking it uses.
    pub fn public_hex(&self) -> String {
        hex(self.key.verifying_key().as_bytes())
    }

    /// A short form of the public key for people: the first 16 hex digits of its SHA-256, in
    /// groups of four.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public_hex())
    }

    /// An Ed25519 signature of `msg`, base64url.
    pub fn sign(&self, msg: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(self.key.sign(msg).to_bytes())
    }

    /// Sign a JSON document in place: sets `key` (this installation's public key) and `sig` (over
    /// the canonical form of everything but `sig`). An object is required.
    pub fn sign_json(&self, doc: &mut Value) -> Result<()> {
        let obj = doc
            .as_object_mut()
            .context("only a JSON object can be signed")?;
        obj.remove("sig");
        obj.insert("key".into(), Value::String(self.public_hex()));
        let sig = self.sign(&canonical(doc));
        doc.as_object_mut()
            .expect("checked above")
            .insert("sig".into(), Value::String(sig));
        Ok(())
    }
}

/// The short form of a public key (see `Signer::fingerprint`).
pub fn fingerprint(public_hex: &str) -> String {
    let h = hex(&Sha256::digest(public_hex.as_bytes()));
    [&h[0..4], &h[4..8], &h[8..12], &h[12..16]].join("-")
}

/// Check an Ed25519 signature made by `public_hex`'s key.
pub fn verify(public_hex: &str, msg: &[u8], sig: &str) -> Result<()> {
    let pk: [u8; 32] = unhex(public_hex)
        .context("the key isn't hex")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("the key isn't 32 bytes"))?;
    let vk = VerifyingKey::from_bytes(&pk).context("that isn't an Ed25519 public key")?;
    let sig = URL_SAFE_NO_PAD
        .decode(sig)
        .ok()
        .and_then(|s| Signature::from_slice(&s).ok())
        .context("the signature is damaged")?;
    vk.verify_strict(msg, &sig)
        .map_err(|_| anyhow::anyhow!("the signature doesn't match: changed after it was signed"))
}

/// Check a document signed by `Signer::sign_json`; the public key that signed it.
pub fn verify_json(doc: &Value) -> Result<String> {
    let obj = doc.as_object().context("not a signed document")?;
    let key = obj
        .get("key")
        .and_then(Value::as_str)
        .context("no key: not a signed document")?
        .to_string();
    let sig = obj
        .get("sig")
        .and_then(Value::as_str)
        .context("no signature: not a signed document")?
        .to_string();
    let mut unsigned = doc.clone();
    unsigned.as_object_mut().expect("an object").remove("sig");
    verify(&key, &canonical(&unsigned), &sig)?;
    Ok(key)
}

/// The canonical form of a JSON value: object keys in byte order, no spaces, strings and numbers
/// as serde_json writes them. Equal documents give equal bytes, whatever order they were built in.
pub fn canonical(v: &Value) -> Vec<u8> {
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).expect("a string"));
                    out.push(':');
                    write(&m[*k], out);
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(x, out);
                }
                out.push(']');
            }
            other => out.push_str(&serde_json::to_string(other).expect("a JSON value")),
        }
    }
    let mut out = String::new();
    write(v, &mut out);
    out.into_bytes()
}

/// SHA-256 of bytes, hex.
pub fn sha256_hex(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_key_is_made_once_and_kept_private() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/signing.key");
        let a = Signer::load_or_create(&p).unwrap();
        let b = Signer::load_or_create(&p).unwrap();
        assert_eq!(
            a.public_hex(),
            b.public_hex(),
            "the same key the second time"
        );
        assert_eq!(a.public_hex().len(), 64);
        assert_eq!(a.fingerprint().len(), 19);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::write(&p, "not a key").unwrap();
        assert!(Signer::load_or_create(&p).is_err());
    }

    #[test]
    fn a_signed_document_checks_and_any_change_shows() {
        let dir = tempfile::tempdir().unwrap();
        let s = Signer::load_or_create(&dir.path().join("k")).unwrap();
        let mut doc =
            json!({ "drive": "/media/old", "files": 3, "nested": { "b": 2, "a": [1, "x"] } });
        s.sign_json(&mut doc).unwrap();
        assert_eq!(verify_json(&doc).unwrap(), s.public_hex());
        // reformatted and with its keys in another order: still the same document
        let text = serde_json::to_string_pretty(&doc).unwrap();
        let back: Value = serde_json::from_str(&text).unwrap();
        assert!(verify_json(&back).is_ok());
        // changed, or signed by someone else, or unsigned: refused
        let mut changed = doc.clone();
        changed["files"] = json!(4);
        assert!(verify_json(&changed).is_err());
        let other = Signer::load_or_create(&dir.path().join("other")).unwrap();
        let mut swapped = doc.clone();
        swapped["key"] = json!(other.public_hex());
        assert!(verify_json(&swapped).is_err());
        assert!(verify_json(&json!({ "files": 3 })).is_err());
    }

    #[test]
    fn canonical_bytes_dont_depend_on_order() {
        let a: Value =
            serde_json::from_str(r#"{"b":1,"a":{"y":[2,{"q":1,"p":0}],"x":"é"}}"#).unwrap();
        let b: Value =
            serde_json::from_str(r#"{"a":{"x":"é","y":[2,{"p":0,"q":1}]},"b":1}"#).unwrap();
        assert_eq!(canonical(&a), canonical(&b));
        assert_eq!(
            String::from_utf8(canonical(&a)).unwrap(),
            r#"{"a":{"x":"é","y":[2,{"p":0,"q":1}]},"b":1}"#
        );
    }
}
