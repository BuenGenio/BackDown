//! Community, Pro, Team, Forensic: which feature needs which licence, in one table.
//!
//! `FEATURES` below is the one place that says what's paid. The web server checks it before it
//! answers (`gate_endpoint`: a 402 that says what the feature does and how to unlock it), the CLI
//! before it runs a command (`gate_command`), and the browser draws its "Pro" badges and
//! explanations from `GET /api/features` (`features_json`).
//!
//! Policy while BackDown is a preview: a pre-release build (a version with -alpha, -beta or -rc)
//! unlocks everything and says so ("Pro features are free during the preview"), so nothing is ever
//! locked before a real release. That switch is `PREVIEW_UNLOCKS_ALL`. A licence key (license.rs)
//! decides the tier; without one it's Community. Setting BACKDOWN_PRETEND_RELEASE=1 makes a preview
//! build behave like a release (it can only lock, never unlock), to try the locked experience.
//!
//! The free tier answers "what do I have and what's duplicated" completely (principle 4 in
//! docs/ROADMAP.md): indexing, hashing, duplicates, the safe-to-wipe check, browsing, mirrors,
//! recognition and the report stay Community.

use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::license::Licence;

/// A pre-release build unlocks every feature (the preview policy). Set to false to lock Pro
/// features in previews too.
pub const PREVIEW_UNLOCKS_ALL: bool = true;

pub const PRICING_URL: &str = "https://backdown.io/pricing";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Community,
    Pro,
    Team,
    Forensic,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Tier::Community => "Community",
            Tier::Pro => "Pro",
            Tier::Team => "Team",
            Tier::Forensic => "Forensic",
        }
    }

    /// The tier a licence's `product` claim buys.
    fn of_product(product: &str) -> Option<Tier> {
        match product {
            "pro" | "supporter" => Some(Tier::Pro),
            "team" => Some(Tier::Team),
            "forensic" => Some(Tier::Forensic),
            _ => None,
        }
    }
}

pub struct Feature {
    pub key: &'static str,
    /// as the pricing page names it (marketing/i18n/en.json, pricing.*)
    pub name: &'static str,
    /// what it does for you, in a sentence (shown where it's locked)
    pub unlocks: &'static str,
    pub tier: Tier,
    /// CLI subcommands it covers
    pub commands: &'static [&'static str],
    /// web API paths it covers (the browser views that use them show the explanation)
    pub endpoints: &'static [&'static str],
    /// browser views that carry its badge (the sidebar buttons' ids, without `-btn`)
    pub views: &'static [&'static str],
    /// in this version of BackDown (the rest are on the way)
    pub ready: bool,
}

use Tier::{Community, Forensic, Pro, Team};

/// Every feature and the tier that unlocks it: change what's paid here, and only here.
pub const FEATURES: &[Feature] = &[
    // ---- Community: free for good
    Feature {
        key: "inventory",
        name: "Backup, restore and dedup",
        unlocks: "Index every drive, hash what could be a duplicate, list duplicates, check a drive is safe to wipe, keep mirrors.",
        tier: Community,
        commands: &[
            "index",
            "hash",
            "dupes",
            "check",
            "scans",
            "mirror",
            "import-legacy",
            "rescue",
        ],
        endpoints: &[],
        views: &["dupes", "activity", "rescue"],
        ready: true,
    },
    Feature {
        key: "ui",
        name: "CLI and local web UI",
        unlocks: "The command line, the browser with its time slider, and the desktop app.",
        tier: Community,
        commands: &[
            "serve",
            "daemon",
            "open",
            "token",
            "completions",
            "app",
            "status",
            "volumes",
            "service",
            "desktop",
            "update",
            "license",
        ],
        endpoints: &[],
        views: &[],
        ready: true,
    },
    Feature {
        // free for good (decided 29 Sep 2026); never gated
        key: "fast-rescans",
        name: "Rescans in seconds from the drive's change log",
        unlocks: "Rescans from the drive's own change log (macOS FSEvents, the Windows USN journal) and NTFS's file table, with a full walk now and then.",
        tier: Community,
        commands: &[],
        endpoints: &[],
        views: &[],
        ready: true,
    },
    Feature {
        key: "sizes",
        name: "Size analytics",
        unlocks: "Folder sizes, the size map and what changed between scans.",
        tier: Community,
        commands: &[],
        endpoints: &[],
        views: &[],
        ready: true,
    },
    Feature {
        key: "recognition",
        name: "Basic recognition",
        unlocks: "What's on each drive: mail stores, backups, phones, systems, repositories, libraries.",
        tier: Community,
        commands: &["entities"],
        endpoints: &[],
        views: &["entities"],
        ready: true,
    },
    Feature {
        key: "export",
        name: "Markdown and JSON export",
        unlocks: "The report as HTML or Markdown, and --json from every command.",
        tier: Community,
        commands: &["report"],
        endpoints: &[],
        views: &[],
        ready: true,
    },
    // ---- Pro
    Feature {
        key: "provenance",
        name: "Full recognition and provenance",
        unlocks: "Where each thing came from: the host, domain, device or person, read from backup scripts, plists, git remotes and dump headers.",
        tier: Pro,
        commands: &[],
        endpoints: &[],
        views: &[],
        ready: true,
    },
    Feature {
        key: "everywhere",
        name: "Dedup across every source",
        unlocks: "How much of each drive, folder and recognised thing already exists elsewhere, and where.",
        tier: Pro,
        commands: &["overlaps"],
        endpoints: &["/api/overlaps"],
        views: &[],
        ready: true,
    },
    Feature {
        key: "reclaim",
        name: "Reclaim simulator",
        unlocks: "Free up: see what freeing drives or folders would delete, move what exists only there, then delete only what has a verified copy.",
        tier: Pro,
        commands: &["wipe"],
        endpoints: &["/api/wipe-assess", "/api/wipe", "/api/wipes", "/api/room"],
        views: &["wipe"],
        ready: true,
    },
    Feature {
        key: "filing",
        name: "Filing suggestions",
        unlocks: "Plan: where everything on the old drives goes in one archive and why, then verified, journaled copies.",
        tier: Pro,
        commands: &["plan", "plans", "apply"],
        endpoints: &[
            "/api/plan",
            "/api/plans",
            "/api/plan-dry",
            "/api/apply",
            // merging a plan's overlapping mailboxes into one
            "/api/plan-merge",
        ],
        views: &["plan"],
        ready: true,
    },
    Feature {
        key: "viewers",
        name: "Data viewers",
        unlocks: "Open databases, spreadsheets, presentations, mailboxes and SQL dumps right in the browser.",
        tier: Pro,
        commands: &[],
        endpoints: &[
            "/api/sqlite",
            "/api/sheet",
            "/api/slides",
            "/api/sqldump",
            "/api/mail",
            "/api/mbox",
            "/api/threads",
            "/api/thread",
            "/api/mail-kind",
        ],
        views: &[],
        ready: true,
    },
    Feature {
        // roadmap v0.6: "Pro tier: multi-machine". Forgetting a machine (`remote remove`,
        // /api/machine-remove) and seeing the list stay free, so nothing gets stuck
        key: "multi-machine",
        name: "Several machines",
        unlocks: "A NAS or another computer with BackDown on it, read over ssh with your keys: browse it, and let its copies count for duplicates, overlaps and freeing drives.",
        tier: Pro,
        commands: &[],
        endpoints: &[
            "/api/machine-ls",
            "/api/machine-file",
            "/api/machine-find",
            "/api/machine-add",
            "/api/machine-test",
            "/api/machine-sync",
            "/api/machine-settings",
        ],
        views: &[],
        ready: true,
    },
    Feature {
        key: "cloud",
        name: "Encrypted cloud targets",
        unlocks: "Mirror the archive to cloud storage, encrypted before it leaves this computer.",
        tier: Pro,
        commands: &[],
        endpoints: &[],
        views: &[],
        ready: false,
    },
    // ---- Team
    Feature {
        key: "fleet",
        name: "Hosted fleet dashboard",
        unlocks: "Every machine's drives in one place.",
        tier: Team,
        commands: &[],
        endpoints: &[],
        views: &[],
        ready: false,
    },
    Feature {
        key: "catalogs",
        name: "Shared catalogs and roles",
        unlocks: "One catalog for a team, with who may see and do what.",
        tier: Team,
        commands: &[],
        endpoints: &[],
        views: &[],
        ready: false,
    },
    Feature {
        key: "health-alerts",
        name: "Drive health alerts",
        unlocks: "Every machine's drive-health warnings gathered in one place. Each machine's own health readings, warnings and notifications are free.",
        tier: Team,
        commands: &[],
        endpoints: &[],
        views: &[],
        ready: false,
    },
    Feature {
        key: "attestations",
        name: "Scheduled attestations",
        unlocks: "Regular signed statements of what each drive holds.",
        tier: Team,
        commands: &[],
        endpoints: &[],
        views: &[],
        ready: false,
    },
    // ---- Forensic
    Feature {
        key: "manifests",
        name: "Signed, tamper-evident manifests",
        unlocks: "Manifests anyone can verify were not changed.",
        tier: Forensic,
        commands: &[],
        endpoints: &[],
        views: &[],
        ready: false,
    },
    Feature {
        key: "custody",
        name: "Chain-of-custody export",
        unlocks: "Every read and copy, exported for evidence handling.",
        tier: Forensic,
        commands: &[],
        endpoints: &[],
        views: &[],
        ready: false,
    },
    Feature {
        // `backdown update --from FOLDER` (update.rs gates it with gate_feature: `update` itself is
        // free); licences are checked offline for everyone
        key: "airgap",
        name: "Air-gapped deployment",
        unlocks: "Licensing and updates without any network: install a signed release from a folder carried over, with `backdown update --from`.",
        tier: Forensic,
        commands: &[],
        endpoints: &[],
        views: &[],
        ready: true,
    },
];

pub fn feature(key: &str) -> Option<&'static Feature> {
    FEATURES.iter().find(|f| f.key == key)
}

/// A pre-release version: 0.5.0-alpha.2, 1.0.0-beta.1, 1.0.0-rc.3.
pub fn is_preview(version: &str) -> bool {
    let pre = version.split_once('-').map_or("", |(_, p)| p);
    ["alpha", "beta", "rc"].iter().any(|t| {
        pre.split(['.', '-'])
            .any(|part| part.to_ascii_lowercase().starts_with(t))
    })
}

/// What this copy of BackDown may do, and why.
#[derive(Debug, Clone)]
pub struct Access {
    pub version: String,
    /// the licence's tier (Community without a valid, current licence)
    pub tier: Tier,
    /// a preview build under the preview policy: everything is unlocked
    pub preview: bool,
    pub licence: Option<Licence>,
    /// a licence file that isn't valid (damaged, expired): why it doesn't count
    pub licence_problem: Option<String>,
}

impl Access {
    /// Work out access from a version, a licence (or why it couldn't be read) and today's date.
    pub fn new(
        version: &str,
        licence: Result<Option<Licence>>,
        today: &str,
        pretend_release: bool,
    ) -> Access {
        let preview = PREVIEW_UNLOCKS_ALL && is_preview(version) && !pretend_release;
        let (licence, mut problem) = match licence {
            Ok(l) => (l, None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };
        let mut tier = Community;
        if let Some(l) = &licence {
            match Tier::of_product(&l.product) {
                None => {
                    problem = Some(format!(
                        "this key is for “{}”, which this BackDown doesn't know",
                        l.product
                    ))
                }
                Some(_) if l.expires.as_deref().is_some_and(|e| e < today) => {
                    problem = Some(format!(
                        "this licence ended on {}; renew it at {PRICING_URL}",
                        l.expires.as_deref().unwrap_or_default()
                    ))
                }
                Some(t) => tier = t,
            }
        }
        Access {
            version: version.to_string(),
            tier,
            preview,
            licence,
            licence_problem: problem,
        }
    }

    pub fn allows(&self, f: &Feature) -> bool {
        self.preview || self.tier >= f.tier
    }

    /// One line: what's unlocked and why.
    pub fn summary(&self) -> String {
        match (self.preview, self.tier) {
            (true, Community) => {
                "Pro features are free during the preview: everything is unlocked.".into()
            }
            (true, t) => format!(
                "{} licence; Pro features are free during the preview anyway.",
                t.name()
            ),
            (false, Community) => format!(
                "BackDown Community: Pro features need a licence ({PRICING_URL}).{}",
                self.licence_problem
                    .as_ref()
                    .map(|p| format!(" The licence here doesn't count: {p}."))
                    .unwrap_or_default()
            ),
            (false, t) => format!("BackDown {}: its features are unlocked.", t.name()),
        }
    }

    /// Why `f` is locked, for the API and the browser: what it is, what it unlocks, how.
    pub fn refusal(&self, f: &Feature) -> Value {
        json!({
            "error": format!("{} is part of BackDown {}.", f.name, f.tier.name()),
            "needs": f.tier,
            "needs_name": f.tier.name(),
            "feature": f.key,
            "name": f.name,
            "unlocks": f.unlocks,
            "tier": self.tier,
            "pricing": PRICING_URL,
            "how": "Add a licence key in Settings, or with `backdown license add <key>`.",
        })
    }
}

/// Access for this binary, now. The licence is read (and its signature checked) once and again
/// only when the file changes, so it's cheap enough to ask per request.
pub fn access() -> Access {
    use std::sync::Mutex;
    use std::time::SystemTime;
    static CACHE: Mutex<Option<(Option<SystemTime>, Access)>> = Mutex::new(None);
    let stamp = std::fs::metadata(crate::license::path())
        .and_then(|m| m.modified())
        .ok();
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((s, a)) = cache.as_ref()
        && *s == stamp
    {
        return a.clone();
    }
    let a = Access::new(
        env!("CARGO_PKG_VERSION"),
        crate::license::load(),
        &today(),
        std::env::var_os("BACKDOWN_PRETEND_RELEASE").is_some_and(|v| !v.is_empty() && v != "0"),
    );
    *cache = Some((stamp, a.clone()));
    a
}

fn today() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    crate::utc(now)[..10].to_string()
}

/// The feature an endpoint belongs to, if it isn't free.
pub fn endpoint_feature(path: &str) -> Option<&'static Feature> {
    FEATURES
        .iter()
        .find(|f| f.tier > Community && f.endpoints.contains(&path))
}

/// Before the server answers `path`: None to go ahead, or the refusal to send (status 402).
pub fn gate_endpoint(path: &str) -> Option<Value> {
    let f = endpoint_feature(path)?;
    let a = access();
    (!a.allows(f)).then(|| a.refusal(f))
}

/// Before the CLI runs a subcommand: an error that explains what's needed, if it's locked.
pub fn gate_command(name: &str) -> Result<()> {
    check_command(&access(), name)
}

fn check_command(a: &Access, name: &str) -> Result<()> {
    let Some(f) = FEATURES
        .iter()
        .find(|f| f.tier > Community && f.commands.contains(&name))
    else {
        return Ok(());
    };
    check(a, f, &format!("backdown {name}"))
}

/// Before the CLI does something that's part of feature `key` without being a subcommand of its
/// own (like `update --from`): the same explanation if it's locked. `what` is how it was asked for.
pub fn gate_feature(key: &str, what: &str) -> Result<()> {
    match feature(key) {
        Some(f) => check(&access(), f, what),
        None => Ok(()),
    }
}

fn check(a: &Access, f: &Feature, what: &str) -> Result<()> {
    if a.allows(f) {
        return Ok(());
    }
    bail!(
        "`{what}` ({}) is part of BackDown {}: {}\n\
         Add a licence key with `backdown license add <key>` (see {PRICING_URL}).{}",
        f.name,
        f.tier.name(),
        f.unlocks,
        a.licence_problem
            .as_ref()
            .map(|p| format!("\nThe licence here doesn't count: {p}."))
            .unwrap_or_default()
    )
}

/// Strip what `provenance` covers from recognised things, if it's locked: where each came from.
pub fn redact_provenance(v: &mut Value) {
    let a = access();
    if feature("provenance").is_none_or(|f| a.allows(f)) {
        return;
    }
    for root in v["roots"].as_array_mut().into_iter().flatten() {
        for e in root["entities"].as_array_mut().into_iter().flatten() {
            if e["provenance"].as_array().is_some_and(|p| !p.is_empty()) {
                e["provenance"] = json!([]);
                e["provenance_locked"] = json!(true);
            }
        }
    }
}

/// The same for the CLI's entities and both reports, before they're printed or written.
pub fn strip_provenance<'a>(
    list: impl IntoIterator<Item = &'a mut backdown_core::recognise::RootEntities>,
) {
    let a = access();
    if feature("provenance").is_none_or(|f| a.allows(f)) {
        return;
    }
    for r in list {
        for e in &mut r.entities {
            e.provenance.clear();
        }
    }
}

/// GET /api/features: the tier, the preview policy, the licence and every feature with its state.
pub fn features_json() -> Value {
    let a = access();
    json!({
        "version": a.version,
        "tier": a.tier,
        "tier_name": a.tier.name(),
        "preview": a.preview,
        "summary": a.summary(),
        "note": a.preview.then_some("Pro features are free during the preview"),
        "licence": a.licence,
        "licence_problem": a.licence_problem,
        "licence_file": crate::license::path().display().to_string(),
        "pricing": PRICING_URL,
        "features": FEATURES.iter().map(|f| json!({
            "key": f.key, "name": f.name, "unlocks": f.unlocks, "tier": f.tier,
            "tier_name": f.tier.name(), "ready": f.ready, "allowed": a.allows(f),
            "views": f.views, "commands": f.commands,
        })).collect::<Vec<_>>(),
    })
}

/// `backdown license features`: the table, with what this copy may use.
pub fn print_features() {
    let a = access();
    println!("{}\n", a.summary());
    for t in [Community, Pro, Team, Forensic] {
        println!("{}", t.name());
        for f in FEATURES.iter().filter(|f| f.tier == t) {
            let state = match (f.ready, a.allows(f)) {
                (false, _) => "coming",
                (true, true) => "yes",
                (true, false) => "locked",
            };
            let cmds = if f.commands.is_empty() {
                String::new()
            } else {
                format!("  ({})", f.commands.join(", "))
            };
            println!("  {state:<7} {}{cmds}", f.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn licence(product: &str, expires: Option<&str>) -> Licence {
        Licence {
            v: 1,
            id: "TEST0-AAAAA-BBBBB-CCCCC".into(),
            product: product.into(),
            kind: "order".into(),
            seats: 1,
            email: "test@example.com".into(),
            name: None,
            issued: "2026-09-28".into(),
            expires: expires.map(String::from),
        }
    }

    #[test]
    fn preview_versions() {
        for v in ["0.2.0-alpha.4", "1.0.0-beta.1", "1.0.0-rc.2", "0.5.0-RC1"] {
            assert!(is_preview(v), "{v}");
        }
        for v in ["0.5.0", "1.0.0", "1.2.3+build.5", "1.0.0-hotfix.1"] {
            assert!(!is_preview(v), "{v}");
        }
    }

    #[test]
    fn a_preview_unlocks_everything_without_a_licence() {
        let a = Access::new("0.5.0-alpha.1", Ok(None), "2026-10-01", false);
        assert!(a.preview);
        assert_eq!(a.tier, Community);
        assert!(FEATURES.iter().all(|f| a.allows(f)));
        assert!(a.summary().contains("free during the preview"));
        assert!(check_command(&a, "plan").is_ok());
        assert_eq!(
            features_state(&a),
            FEATURES.iter().map(|_| true).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_release_locks_pro_without_a_licence() {
        let a = Access::new("1.0.0", Ok(None), "2026-10-01", false);
        assert!(!a.preview);
        let pro = feature("filing").unwrap();
        assert!(!a.allows(pro));
        assert!(a.allows(feature("inventory").unwrap()));
        assert!(a.allows(feature("recognition").unwrap()));
        let err = check_command(&a, "plan").unwrap_err().to_string();
        assert!(
            err.contains("Filing suggestions") && err.contains("Pro"),
            "{err}"
        );
        assert!(err.contains("backdown license add"), "{err}");
        // what stays free always runs
        for c in [
            "index", "dupes", "check", "serve", "report", "entities", "app",
        ] {
            assert!(check_command(&a, c).is_ok(), "{c}");
        }
        let r = a.refusal(pro);
        assert_eq!(r["needs"], "pro");
        assert_eq!(r["feature"], "filing");
        assert!(r["unlocks"].as_str().unwrap().contains("archive"));
    }

    #[test]
    fn pretending_to_be_a_release_only_locks() {
        let a = Access::new("0.5.0-alpha.1", Ok(None), "2026-10-01", true);
        assert!(!a.preview && !a.allows(feature("reclaim").unwrap()));
    }

    #[test]
    fn a_licence_unlocks_its_tier_and_below() {
        let pro = Access::new("1.0.0", Ok(Some(licence("pro", None))), "2026-10-01", false);
        assert_eq!(pro.tier, Pro);
        assert!(pro.allows(feature("filing").unwrap()));
        assert!(!pro.allows(feature("fleet").unwrap()));
        let team = Access::new(
            "1.0.0",
            Ok(Some(licence("team", None))),
            "2026-10-01",
            false,
        );
        assert!(team.allows(feature("filing").unwrap()) && team.allows(feature("fleet").unwrap()));
        assert!(!team.allows(feature("custody").unwrap()));
    }

    #[test]
    fn an_expired_or_unknown_licence_is_community_with_a_reason() {
        let old = Access::new(
            "1.0.0",
            Ok(Some(licence("pro", Some("2026-09-30")))),
            "2026-10-01",
            false,
        );
        assert_eq!(old.tier, Community);
        assert!(old.licence_problem.unwrap().contains("ended on 2026-09-30"));
        let still = Access::new(
            "1.0.0",
            Ok(Some(licence("pro", Some("2026-10-01")))),
            "2026-10-01",
            false,
        );
        assert_eq!(still.tier, Pro);
        let odd = Access::new(
            "1.0.0",
            Ok(Some(licence("platinum", None))),
            "2026-10-01",
            false,
        );
        assert_eq!(odd.tier, Community);
        let broken = Access::new(
            "1.0.0",
            Err(anyhow::anyhow!("the key is damaged")),
            "2026-10-01",
            false,
        );
        assert_eq!(broken.tier, Community);
        assert!(broken.summary().contains("damaged"));
    }

    #[test]
    fn the_table_is_consistent() {
        let mut keys = std::collections::HashSet::new();
        let mut endpoints = std::collections::HashSet::new();
        let mut commands = std::collections::HashSet::new();
        for f in FEATURES {
            assert!(keys.insert(f.key), "feature {} twice", f.key);
            for e in f.endpoints {
                assert!(e.starts_with("/api/"), "{e}");
                assert!(endpoints.insert(*e), "endpoint {e} in two features");
            }
            for c in f.commands {
                assert!(commands.insert(*c), "command {c} in two features");
            }
            // a feature that isn't built can't be gating anything yet
            assert!(
                f.ready || (f.commands.is_empty() && f.endpoints.is_empty()),
                "{}",
                f.key
            );
        }
        assert_eq!(
            feature("fast-rescans").unwrap().tier,
            Community,
            "decided: free"
        );
        // the free tier answers "what do I have and what's duplicated" completely
        for c in ["index", "hash", "dupes", "check", "entities", "report"] {
            assert!(
                FEATURES
                    .iter()
                    .any(|f| f.tier == Community && f.commands.contains(&c)),
                "{c} must stay free"
            );
        }
        assert_eq!(endpoint_feature("/api/plan").unwrap().key, "filing");
        assert!(endpoint_feature("/api/dupes").is_none());
    }

    #[test]
    fn updating_from_a_folder_is_forensic() {
        let f = feature("airgap").unwrap();
        assert!(f.ready && f.tier == Forensic);
        let what = "backdown update --from";
        let none = Access::new("1.0.0", Ok(None), "2026-10-01", false);
        let err = check(&none, f, what).unwrap_err().to_string();
        assert!(
            err.contains(
                "`backdown update --from` (Air-gapped deployment) is part of BackDown Forensic"
            ),
            "{err}"
        );
        assert!(err.contains("backdown license add"), "{err}");
        let team = Access::new(
            "1.0.0",
            Ok(Some(licence("team", None))),
            "2026-10-01",
            false,
        );
        assert!(check(&team, f, what).is_err());
        let forensic = Access::new(
            "1.0.0",
            Ok(Some(licence("forensic", None))),
            "2026-10-01",
            false,
        );
        assert!(check(&forensic, f, what).is_ok());
        let preview = Access::new("1.0.0-beta.4", Ok(None), "2026-10-01", false);
        assert!(check(&preview, f, what).is_ok());
        // `update` itself stays free
        assert!(check_command(&none, "update").is_ok());
    }

    fn features_state(a: &Access) -> Vec<bool> {
        FEATURES.iter().map(|f| a.allows(f)).collect()
    }
}
