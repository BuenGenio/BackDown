use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::VerifyingKey;
use serde_json::Value;

use super::*;

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

#[test]
fn versions_order_like_semver() {
    let order = [
        "0.1.9",
        "0.2.0-alpha.1",
        "0.2.0-alpha.4",
        "0.2.0-alpha.10",
        "0.2.0-beta",
        "0.2.0-beta.2",
        "0.2.0-rc.1",
        "0.2.0",
        "0.2.1",
        "0.10.0",
        "1.0.0",
    ];
    for w in order.windows(2) {
        assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
    }
    assert_eq!(v("v1.2.3+build.9"), v("1.2.3"));
    assert!(Version::parse("1.2").is_none());
    assert!(Version::parse("1.2.3.4").is_none());
    assert!(Version::parse("1.2.3-").is_none());
    assert!(Version::parse("banana").is_none());
    assert!(current() == v(env!("CARGO_PKG_VERSION")));
}

fn release(tag: &str, pre: bool, plats: &[&str]) -> Release {
    let mut assets: Vec<Asset> = plats
        .iter()
        .map(|p| Asset {
            name: archive_name(tag, p),
            url: format!("https://example.com/{tag}/{p}"),
            size: 100,
        })
        .collect();
    assets.push(Asset {
        name: SUMS.into(),
        url: format!("https://example.com/{tag}/sums"),
        size: 10,
    });
    Release {
        tag: tag.into(),
        prerelease: pre,
        url: format!("https://example.com/releases/{tag}"),
        published_at: None,
        assets,
    }
}

#[test]
fn picking_the_newest_for_a_channel_and_platform() {
    let all = [
        "linux-x86_64",
        "linux-arm64",
        "macos-universal",
        "windows-x86_64",
    ];
    let list = vec![
        release("v0.2.0-alpha.4", true, &all),
        release("v0.3.0", false, &all),
        release("v0.4.0-beta.1", true, &all),
        release("v0.3.1", false, &["linux-x86_64"]), // a Linux-only fix
    ];
    let (ver, r) = pick(&list, Channel::Stable, "macos-universal").unwrap();
    assert_eq!((ver, r.tag.as_str()), (v("0.3.0"), "v0.3.0"));
    assert_eq!(
        pick(&list, Channel::Stable, "linux-x86_64").unwrap().1.tag,
        "v0.3.1"
    );
    assert_eq!(
        pick(&list, Channel::Preview, "linux-arm64").unwrap().1.tag,
        "v0.4.0-beta.1"
    );
    // a release without checksums is never offered
    let mut unsummed = release("v9.0.0", false, &all);
    unsummed.assets.retain(|a| a.name != SUMS);
    assert_eq!(pick(&[unsummed], Channel::Stable, "linux-arm64"), None);
    // a pre-release tag counts as a preview even if it isn't marked as one
    let sneaky = release("v0.5.0-rc.1", false, &all);
    assert!(pick(&[sneaky], Channel::Stable, "linux-arm64").is_none());
}

#[test]
fn github_release_json() {
    let v: Value = serde_json::json!([
        { "tag_name": "v0.3.0", "prerelease": false, "draft": false, "html_url": "https://github.com/x/y/releases/tag/v0.3.0",
          "published_at": "2026-10-01T10:00:00Z",
          "assets": [{ "name": "SHA256SUMS.txt", "browser_download_url": "https://github.com/x/y/releases/download/v0.3.0/SHA256SUMS.txt", "size": 400 }] },
        { "tag_name": "v0.4.0", "draft": true, "assets": [] },
        { "no": "tag" }
    ]);
    let r = parse_releases(&v);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].tag, "v0.3.0");
    assert_eq!(r[0].assets[0].size, 400);
}

#[test]
fn checksums_file() {
    let sums = "c6418ceda15c13829e11d70ad3ed8d8c7cd5c7a5d7d564b6af737d5a1b12144b  backdown-v0.2.0-alpha.4-linux-x86_64.tar.gz\n\
                421FB22C42F878B68DDF84EF856285808FB60E165CFCA719F34C2EB562FD451C *backdown-v0.2.0-alpha.4-linux-arm64.tar.gz\n\
                nothex  other\n";
    assert_eq!(
        expected_sum(sums, "backdown-v0.2.0-alpha.4-linux-arm64.tar.gz").unwrap(),
        "421fb22c42f878b68ddf84ef856285808fb60e165cfca719f34c2eb562fd451c"
    );
    assert!(expected_sum(sums, "backdown-v0.2.0-alpha.4-linux-x86_64.tar.gz").is_some());
    assert!(expected_sum(sums, "other").is_none());
    assert!(expected_sum(sums, "backdown-v0.2.0-alpha.4-windows-x86_64.zip").is_none());
}

#[test]
fn only_https_except_to_this_machine() {
    assert_eq!(allowed("https://api.github.com/x").unwrap(), "=https");
    assert_eq!(allowed("http://127.0.0.1:8080/x").unwrap(), "=http");
    assert_eq!(allowed("http://localhost/x").unwrap(), "=http");
    assert!(allowed("http://example.com/x").is_err());
    assert!(allowed("http://127.0.0.1.example.com/x").is_err());
    assert!(allowed("ftp://127.0.0.1/x").is_err());
    assert!(allowed("file:///etc/passwd").is_err());
}

#[test]
fn headers_of_the_last_response() {
    let dump = "HTTP/1.1 302 Found\r\nLocation: https://x/y\r\n\r\nHTTP/2 403 \r\nX-RateLimit-Remaining: 0\r\nx-ratelimit-reset: 1790003600\r\n\r\n";
    let (status, h) = parse_headers(dump);
    assert_eq!(status, 403);
    let f = Fetched {
        status,
        headers: h,
        body: Vec::new(),
    };
    assert!(f.limited());
    assert_eq!(f.limit_reset(1_790_000_000), 1_790_003_600);
    let retry = Fetched {
        status: 429,
        headers: vec![("retry-after".into(), "120".into())],
        body: Vec::new(),
    };
    assert!(retry.limited());
    assert_eq!(retry.limit_reset(1000), 1120);
    let denied = Fetched {
        status: 403,
        headers: vec![("x-ratelimit-remaining".into(), "42".into())],
        body: Vec::new(),
    };
    assert!(!denied.limited());
}

#[test]
fn archives_are_named_as_published() {
    assert_eq!(
        archive_name("v0.2.0-alpha.4", "windows-x86_64"),
        "backdown-v0.2.0-alpha.4-windows-x86_64.zip"
    );
    assert_eq!(
        archive_name("v0.2.0-alpha.4", "macos-universal"),
        "backdown-v0.2.0-alpha.4-macos-universal.tar.gz"
    );
}

#[test]
fn package_managed_binaries_are_left_alone() {
    if std::env::var_os("APPIMAGE").is_some() {
        return;
    }
    assert!(
        not_replaceable(Path::new("/usr/bin/backdown"))
            .unwrap()
            .contains("package manager")
    );
    assert!(not_replaceable(Path::new("/opt/homebrew/Cellar/backdown/1/bin/backdown")).is_some());
    assert!(not_replaceable(Path::new("/home/someone/.local/bin/backdown")).is_none());
    assert!(
        not_replaceable(Path::new(
            "/Applications/BackDown.app/Contents/MacOS/backdown"
        ))
        .unwrap()
        .contains(".dmg")
    );
}

// ---- signatures, with throwaway keys made here (never the release key)

use ed25519_dalek::{Signer as _, SigningKey};

fn sign(k: &SigningKey, data: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(k.sign(data).to_bytes())
}

const SUMS_TEXT: &[u8] = b"c6418ceda15c13829e11d70ad3ed8d8c7cd5c7a5d7d564b6af737d5a1b12144b  backdown-v1.0.0-linux-x86_64.tar.gz\n";

#[test]
fn a_signature_over_the_exact_sums_passes() {
    let k = SigningKey::from_bytes(&[7; 32]);
    let pk = k.verifying_key().to_bytes();
    let sig = sign(&k, SUMS_TEXT);
    verify_sums(SUMS_TEXT, &sig, &pk).unwrap();
    // as sign-release.mjs writes it (a newline after), or with base64 padding
    verify_sums(SUMS_TEXT, &format!("{sig}\n"), &pk).unwrap();
    verify_sums(SUMS_TEXT, &format!("{sig}=="), &pk).unwrap();
}

#[test]
fn changed_sums_or_a_changed_signature_fail() {
    let k = SigningKey::from_bytes(&[7; 32]);
    let pk = k.verifying_key().to_bytes();
    let sig = sign(&k, SUMS_TEXT);
    // one hex digit of a hash changed
    let mut changed = SUMS_TEXT.to_vec();
    changed[0] = b'd';
    let e = verify_sums(&changed, &sig, &pk).unwrap_err().to_string();
    assert!(e.contains("doesn't match SHA256SUMS.txt"), "{e}");
    // a line added
    let mut longer = SUMS_TEXT.to_vec();
    longer.extend_from_slice(b"00  extra\n");
    assert!(verify_sums(&longer, &sig, &pk).is_err());
    // the signature changed: a flipped bit, cut short, or not base64 at all
    let mut raw = URL_SAFE_NO_PAD.decode(&sig).unwrap();
    raw[10] ^= 1;
    assert!(verify_sums(SUMS_TEXT, &URL_SAFE_NO_PAD.encode(&raw), &pk).is_err());
    let e = verify_sums(SUMS_TEXT, &sig[..40], &pk)
        .unwrap_err()
        .to_string();
    assert!(e.contains("damaged"), "{e}");
    assert!(verify_sums(SUMS_TEXT, "not a signature!", &pk).is_err());
    assert!(verify_sums(SUMS_TEXT, "", &pk).is_err());
}

#[test]
fn a_signature_by_another_key_fails() {
    let k = SigningKey::from_bytes(&[7; 32]);
    let other = SigningKey::from_bytes(&[8; 32]);
    let sig = sign(&other, SUMS_TEXT);
    assert!(verify_sums(SUMS_TEXT, &sig, &k.verifying_key().to_bytes()).is_err());
    // and nothing a test key signs passes as a release
    assert!(verify_sums(SUMS_TEXT, &sign(&k, SUMS_TEXT), &RELEASE_KEY).is_err());
    assert!(VerifyingKey::from_bytes(&RELEASE_KEY).is_ok());
}

#[test]
fn releases_need_a_signature_from_beta_4_on() {
    for old in [
        "0.2.0-alpha.4",
        "1.0.0-beta.1",
        "1.0.0-beta.3",
        "1.0.0-beta.3.1",
    ] {
        assert!(!needs_signature(&v(old)), "{old}");
    }
    for new in ["1.0.0-beta.4", "1.0.0-rc.1", "1.0.0", "1.2.0", "9.9.9"] {
        assert!(needs_signature(&v(new)), "{new}");
    }
    assert!(UNSIGNED.contains(&format!("v{FIRST_SIGNED}")));
}

#[test]
fn a_bundle_lists_plain_names_of_one_release() {
    let sums = format!(
        "{h}  backdown-v1.0.0-linux-arm64.tar.gz\n{h}  backdown-v1.0.0-windows-x86_64.zip\n{h}  backdown-io_1.0.0_arm64.deb\n{h}  ../escape.tar.gz\n{h}  sub/dir.tar.gz\n",
        h = "ab".repeat(32)
    );
    let l = listed(&sums);
    assert_eq!(
        l.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        [
            "backdown-v1.0.0-linux-arm64.tar.gz",
            "backdown-v1.0.0-windows-x86_64.zip",
            "backdown-io_1.0.0_arm64.deb"
        ]
    );
    assert_eq!(bundle_tag(&l).unwrap().as_deref(), Some("v1.0.0"));
    let two = listed(&format!(
        "{sums}{}  backdown-v1.0.1-linux-arm64.tar.gz\n",
        "cd".repeat(32)
    ));
    assert!(
        bundle_tag(&two)
            .unwrap_err()
            .to_string()
            .contains("more than one release")
    );
    assert_eq!(bundle_tag(&listed("")).unwrap(), None);
}

#[test]
fn package_managed_copies_are_told_what_to_do_with_the_carried_files() {
    if std::env::var_os("APPIMAGE").is_some() {
        return;
    }
    let deb_arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        a => a,
    };
    let deb = format!("backdown-io_1.0.0_{deb_arch}.deb");
    let files = vec![SUMS.to_string(), deb.clone()];
    let (why, file) =
        not_replaceable_offline(Path::new("/usr/bin/backdown"), Path::new("."), &files).unwrap();
    assert!(why.contains(&format!("sudo apt install ./{deb}")), "{why}");
    assert!(why.contains("package manager"), "{why}");
    assert_eq!(file.as_deref(), Some(deb.as_str()));
    // an absolute folder, with a space in it, and no .deb there
    let (why, file) = not_replaceable_offline(
        Path::new("/usr/bin/backdown"),
        Path::new("/media/USB STICK"),
        &[SUMS.to_string()],
    )
    .unwrap();
    assert!(why.contains("carry it over too"), "{why}");
    assert_eq!(file, None);
    assert_eq!(
        deb_path(Path::new("/media/USB STICK/x.deb")),
        "'/media/USB STICK/x.deb'"
    );
    assert_eq!(deb_path(Path::new("usb/x.deb")), "./usb/x.deb");
    let (why, _) = not_replaceable_offline(
        Path::new("/Applications/BackDown.app/Contents/MacOS/backdown"),
        Path::new("/Volumes/USB"),
        &["BackDown-1.0.0.dmg".to_string()],
    )
    .unwrap();
    assert!(why.contains("/Volumes/USB/BackDown-1.0.0.dmg"), "{why}");
    assert!(
        not_replaceable_offline(
            Path::new("/home/someone/.local/bin/backdown"),
            Path::new("."),
            &files
        )
        .is_none()
    );
}

#[test]
fn a_zip_entry_comes_out_whole() {
    let dir = tempfile::tempdir().unwrap();
    let z = dir.path().join("a.zip");
    {
        let f = std::fs::File::create(&z).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.start_file("README.txt", opts).unwrap();
        std::io::Write::write_all(&mut w, b"read me").unwrap();
        w.start_file("backdown.exe", opts).unwrap();
        std::io::Write::write_all(&mut w, &b"MZ program ".repeat(1000)).unwrap();
        w.finish().unwrap();
    }
    let out = dir.path().join("out.exe");
    unzip_one(&z, "backdown.exe", &out).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), b"MZ program ".repeat(1000));
    assert!(unzip_one(&z, "missing.exe", &out).is_err());
}
