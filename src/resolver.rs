use serde::Deserialize;
use std::path::PathBuf;

/// A resolver describes how to find the latest version of a vendor's
/// products from public web pages. Declarative on purpose: fixing a broken
/// resolver means editing a TOML file, not shipping a new binary.
#[derive(Deserialize)]
pub struct ResolverFile {
    pub vendor: String,
    #[serde(default)]
    pub homepage: Option<String>,
    /// The vendor distributes updates through their own download-manager
    /// app; fetch recommends launching it instead of probing per-product.
    #[serde(default)]
    pub download_via: Option<String>,
    /// Stated reason automated checking is impossible for this vendor
    /// ("behind login", "JS-rendered site", "vendor updater required").
    /// Documentation, telemetry, and walkthrough trigger in one.
    #[serde(default)]
    pub skip: Option<String>,
    #[serde(default)]
    pub manual: Option<Manual>,
    #[serde(default, rename = "bundle", alias = "product")]
    pub bundles: Vec<BundleResolver>,
}

/// How a human gets the download when automation can't.
#[derive(Deserialize, Clone)]
pub struct Manual {
    #[serde(default)]
    pub login: Option<String>,
    #[serde(default)]
    pub steps: Option<String>,
}

#[derive(Deserialize, Clone)]
pub struct BundleResolver {
    /// Product name as it appears in the catalog (case-insensitive).
    pub name: String,
    /// Page to fetch. Pages are fetched once per run even if shared.
    /// For `github_release`, this is the repo ("owner/repo" or a github.com
    /// URL); for `sparkle`, the appcast URL; for `header`, the URL whose
    /// redirect encodes the version.
    pub page: String,
    /// How to extract the version. Default: "page_regex".
    /// One of: page_regex | json | github_release | sparkle | header.
    #[serde(default)]
    pub strategy: Option<String>,
    /// First capture group must be the version string. Required for
    /// page_regex and header; optional refinement for json/sparkle.
    #[serde(default)]
    pub version_regex: Option<String>,
    /// Dot-path into a JSON document for the `json` strategy,
    /// e.g. "results.0.version".
    #[serde(default)]
    pub json_path: Option<String>,
    /// Candidate versions matching this are skipped (prereleases). Default:
    /// (?i)alpha|beta|rc|nightly|dev|demo
    #[serde(default)]
    pub exclude_regex: Option<String>,
    /// Provenance when the page is not the vendor's own site
    /// ("Plugin Boutique", "Rekkerd"). Surfaced in check and outdated output.
    #[serde(default)]
    pub via: Option<String>,
    /// Versions at or above this are a paid major upgrade to a distinct
    /// product (e.g. RePitch 2 vs RePitch 1). When an installed version below
    /// this is compared against a latest at or above it, outdated reports a
    /// paid upgrade rather than a routine stale update.
    #[serde(default)]
    pub paid_from: Option<String>,
    /// May contain "${version}", substituted after resolution.
    #[serde(default)]
    pub download: Option<String>,
    #[serde(default)]
    pub changelog: Option<String>,
}

/// Resolver sources, most-trusted first: an explicit --resolvers/-env dir,
/// then the user's config dir. The current working directory is deliberately
/// NOT searched: running plugscan inside an untrusted folder must never load
/// that folder's TOMLs.
fn resolver_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(env_dir) = std::env::var("PLUGSCAN_RESOLVERS") {
        dirs.push(PathBuf::from(env_dir));
    }
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(home).join(".config/plugscan/resolvers"));
    }
    dirs
}

fn https_or_drop(url: &Option<String>, ctx: &str) -> Option<String> {
    match url {
        Some(u) if u.starts_with("https://") => Some(crate::util::sanitize_line(u)),
        Some(u) => {
            eprintln!(
                "warning: {ctx}: non-https URL rejected: {}",
                crate::util::sanitize_line(u)
            );
            None
        }
        None => None,
    }
}

/// Enforce the trust rules on a loaded resolver: https-only URLs everywhere,
/// control characters stripped from every field. Products whose fetch page
/// is not https are dropped entirely.
fn validate(mut rf: ResolverFile) -> ResolverFile {
    rf.vendor = crate::util::sanitize_line(&rf.vendor);
    rf.homepage = https_or_drop(&rf.homepage, &rf.vendor);
    rf.download_via = rf.download_via.as_deref().map(crate::util::sanitize_line);
    if let Some(m) = &mut rf.manual {
        m.login = https_or_drop(&m.login, &rf.vendor);
        m.steps = m.steps.as_deref().map(crate::util::sanitize_text);
    }
    let vendor = rf.vendor.clone();
    rf.bundles.retain_mut(|p| {
        p.name = crate::util::sanitize_line(&p.name);
        // github_release accepts an "owner/repo" shorthand that expands to
        // the https API URL; everything else must be https already.
        let gh_shorthand =
            p.strategy.as_deref() == Some("github_release") && !p.page.contains("://");
        if !gh_shorthand && !p.page.starts_with("https://") {
            eprintln!(
                "warning: {vendor} {}: non-https page rejected, product dropped",
                p.name
            );
            return false;
        }
        p.page = crate::util::sanitize_line(&p.page);
        p.via = p.via.as_deref().map(crate::util::sanitize_line);
        p.paid_from = p.paid_from.as_deref().map(crate::util::sanitize_line);
        p.download = https_or_drop(&p.download, &vendor);
        p.changelog = https_or_drop(&p.changelog, &vendor);
        true
    });
    rf
}

pub fn load_all() -> Vec<ResolverFile> {
    let mut out: Vec<ResolverFile> = Vec::new();
    for dir in resolver_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            match std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|s| toml::from_str::<ResolverFile>(&s).map_err(|e| e.to_string()))
            {
                Ok(rf) => {
                    let rf = validate(rf);
                    // Earlier dirs (env, user config) override later ones (shipped).
                    if !out.iter().any(|r| r.vendor == rf.vendor) {
                        out.push(rf);
                    }
                }
                Err(e) => eprintln!("warning: skipping {}: {e}", path.display()),
            }
        }
    }
    out
}
