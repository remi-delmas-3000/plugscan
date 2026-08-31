use crate::db::unix_now;
use crate::resolver::{self, BundleResolver};
use crate::util::sanitize_line;
use regex::Regex;
use rusqlite::{params, Connection};
use std::collections::HashMap;

// Resolver pages were verified with a browser UA; several vendors reject
// unknown agents outright, so the checker identifies the same way, with
// plugscan appended for transparency.
const USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/128.0 Safari/537.36 plugscan/",
    env!("CARGO_PKG_VERSION")
);

/// Candidate versions matching this are prereleases unless a resolver
/// overrides `exclude_regex`.
const DEFAULT_EXCLUDE: &str = r"(?i)alpha|beta|\brc\b|nightly|dev|demo";

pub struct Fetcher {
    agent: ureq::Agent,
    no_redirect: ureq::Agent,
    cache: HashMap<String, Option<String>>,
    // Politeness delay before each real network fetch (not cache hits), so a
    // host serving many same-page bundles is throttled without penalizing the
    // cache. Zero for the interactive debug/new tools.
    fetch_delay: std::time::Duration,
    fetched: bool,
}

fn tls() -> ureq::tls::TlsConfig {
    ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .build()
}

impl Fetcher {
    pub fn new() -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(20)))
            .tls_config(tls())
            .build()
            .into();
        let no_redirect: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(20)))
            .max_redirects(0)
            .http_status_as_error(false)
            .tls_config(tls())
            .build()
            .into();
        Fetcher {
            agent,
            no_redirect,
            cache: HashMap::new(),
            fetch_delay: std::time::Duration::ZERO,
            fetched: false,
        }
    }

    /// Sleep before a real fetch (skipping the very first), so consecutive
    /// network requests to one host are spaced out; cache hits never wait.
    fn throttle(&mut self) {
        if self.fetched && !self.fetch_delay.is_zero() {
            std::thread::sleep(self.fetch_delay);
        }
        self.fetched = true;
    }

    fn page(&mut self, url: &str) -> Option<String> {
        if let Some(cached) = self.cache.get(url) {
            return cached.clone();
        }
        self.throttle();
        let result = self
            .agent
            .get(url)
            .header("User-Agent", USER_AGENT)
            .call()
            .ok()
            .and_then(|mut res| res.body_mut().read_to_string().ok());
        self.cache.insert(url.to_string(), result.clone());
        result
    }

    /// For the `header` strategy: the version hides in the redirect Location
    /// or Content-Disposition of a stable URL; no body needed.
    fn headers(&mut self, url: &str) -> Option<String> {
        self.throttle();
        let res = self
            .no_redirect
            .get(url)
            .header("User-Agent", USER_AGENT)
            .call()
            .ok()?;
        let mut joined = String::new();
        for name in ["location", "content-disposition"] {
            if let Some(v) = res.headers().get(name).and_then(|v| v.to_str().ok()) {
                joined.push_str(v);
                joined.push('\n');
            }
        }
        Some(joined)
    }
}

fn json_at<'a>(v: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut cur = v;
    for seg in path.split('.') {
        cur = match seg.parse::<usize>() {
            Ok(i) => cur.get(i)?,
            Err(_) => cur.get(seg)?,
        };
    }
    Some(cur)
}

fn compile(re: &Option<String>, ctx: &str) -> Result<Option<Regex>, String> {
    match re {
        None => Ok(None),
        Some(s) => Regex::new(s)
            .map(Some)
            .map_err(|e| format!("{ctx}: bad regex: {e}")),
    }
}

/// Extract the latest version for one bundle. Err carries a reason for
/// `resolver debug` and the check summary.
pub fn resolve_version(f: &mut Fetcher, pr: &BundleResolver) -> Result<String, String> {
    let exclude = Regex::new(pr.exclude_regex.as_deref().unwrap_or(DEFAULT_EXCLUDE))
        .map_err(|e| format!("bad exclude_regex: {e}"))?;
    let refine = compile(&pr.version_regex, "version_regex")?;
    let strategy = pr.strategy.as_deref().unwrap_or("page_regex");

    let refine_or_pass = |s: &str| -> Option<String> {
        match &refine {
            Some(re) => re
                .captures(s)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string()),
            None => Some(s.to_string()),
        }
    };

    let version = match strategy {
        "page_regex" => {
            let re = refine.as_ref().ok_or("page_regex requires version_regex")?;
            let body = f.page(&pr.page).ok_or("fetch failed")?;
            re.captures_iter(&body)
                .filter_map(|c| c.get(1))
                .map(|m| m.as_str().to_string())
                .find(|v| !exclude.is_match(v))
                .ok_or("no non-prerelease match on page")?
        }
        "json" => {
            let body = f.page(&pr.page).ok_or("fetch failed")?;
            let doc: serde_json::Value =
                serde_json::from_str(&body).map_err(|e| format!("invalid JSON: {e}"))?;
            let path = pr.json_path.as_deref().ok_or("json requires json_path")?;
            let v = json_at(&doc, path).ok_or("json_path not found")?;
            let raw = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            refine_or_pass(&raw).ok_or("version_regex did not match json value")?
        }
        "github_release" => {
            let repo = pr
                .page
                .trim_start_matches("https://github.com/")
                .trim_end_matches('/');
            let api = format!("https://api.github.com/repos/{repo}/releases/latest");
            let body = f.page(&api).ok_or("github api fetch failed")?;
            let doc: serde_json::Value =
                serde_json::from_str(&body).map_err(|e| format!("invalid JSON: {e}"))?;
            if doc.get("prerelease").and_then(|v| v.as_bool()) == Some(true) {
                return Err("latest release is a prerelease".into());
            }
            let tag = doc
                .get("tag_name")
                .and_then(|v| v.as_str())
                .ok_or("no tag_name")?;
            let stripped = tag.trim_start_matches(['v', 'V']).to_string();
            refine_or_pass(&stripped).ok_or("version_regex did not match tag")?
        }
        "sparkle" => {
            let body = f.page(&pr.page).ok_or("appcast fetch failed")?;
            let candidates = [
                r#"sparkle:shortVersionString="([^"]+)""#,
                r"<sparkle:shortVersionString>([^<]+)<",
                r#"sparkle:version="([^"]+)""#,
                r"<sparkle:version>([^<]+)<",
            ];
            let mut found: Option<String> = None;
            for pat in candidates {
                let re = Regex::new(pat).unwrap();
                let hit: Option<String> = re
                    .captures_iter(&body)
                    .filter_map(|c| c.get(1))
                    .map(|m| m.as_str().to_string())
                    .find(|v| !exclude.is_match(v));
                if hit.is_some() {
                    found = hit;
                    break;
                }
            }
            found.ok_or("no version in appcast")?
        }
        "header" => {
            let re = refine.as_ref().ok_or("header requires version_regex")?;
            let joined = f.headers(&pr.page).ok_or("request failed")?;
            re.captures(&joined)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
                .ok_or("version not in redirect headers")?
        }
        other => return Err(format!("unknown strategy: {other}")),
    };

    let version = sanitize_line(&version);
    if exclude.is_match(&version) {
        return Err(format!("matched prerelease: {version}"));
    }
    if version.is_empty() {
        return Err("empty version".into());
    }
    Ok(version)
}

fn action_url(pr: &BundleResolver, version: &str) -> String {
    let url = pr
        .download
        .clone()
        .or_else(|| pr.changelog.clone())
        .unwrap_or_else(|| pr.page.clone());
    url.replace("${version}", version)
}

/// Grouping key so all requests to one host run on a single thread, in
/// sequence, with a politeness delay: keeps plugscan from hammering any one
/// server (KVR Audio in particular, whose ~114 bundles share one host).
fn host_key(pr: &BundleResolver) -> String {
    if pr.strategy.as_deref() == Some("github_release") {
        return "api.github.com".to_string();
    }
    pr.page
        .strip_prefix("https://")
        .and_then(|s| s.split(['/', '?']).next())
        .unwrap_or(&pr.page)
        .to_string()
}

/// Shared agents built once; each host-group thread clones them (cheap, Arc
/// inside) and keeps its own page cache so a shared page is fetched once.
fn build_agents() -> (ureq::Agent, ureq::Agent) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .tls_config(tls())
        .build()
        .into();
    let no_redirect: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .max_redirects(0)
        .http_status_as_error(false)
        .tls_config(tls())
        .build()
        .into();
    (agent, no_redirect)
}

/// One unit of check work: a resolver entry plus the catalog bundle id it
/// maps to (None when exercising resolvers without a catalog).
pub struct Work<'a> {
    pub vendor: &'a str,
    pub name: &'a str,
    pub via: Option<&'a str>,
    pub pr: &'a BundleResolver,
    pub bundle_id: Option<i64>,
}

pub struct Outcome {
    pub vendor: String,
    pub name: String,
    pub via: Option<String>,
    pub bundle_id: Option<i64>,
    pub result: Result<String, String>,
}

use rayon::prelude::*;
use std::collections::BTreeMap;

/// Fetch all work items concurrently: hosts in parallel (bounded pool),
/// sequential within a host with a politeness delay.
fn run_concurrent(items: Vec<Work>) -> Vec<Outcome> {
    let mut groups: BTreeMap<String, Vec<Work>> = BTreeMap::new();
    for w in items {
        groups.entry(host_key(w.pr)).or_default().push(w);
    }
    let (agent, no_redirect) = build_agents();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(8)
        .build()
        .expect("thread pool");
    pool.install(|| {
        groups
            .into_par_iter()
            .flat_map_iter(|(_host, work)| {
                let mut f = Fetcher {
                    agent: agent.clone(),
                    no_redirect: no_redirect.clone(),
                    cache: HashMap::new(),
                    fetch_delay: std::time::Duration::from_millis(250),
                    fetched: false,
                };
                let mut out = Vec::with_capacity(work.len());
                for w in work.iter() {
                    out.push(Outcome {
                        vendor: w.vendor.to_string(),
                        name: w.name.to_string(),
                        via: w.via.map(str::to_string),
                        bundle_id: w.bundle_id,
                        result: resolve_version(&mut f, w.pr),
                    });
                }
                out
            })
            .collect()
    })
}

pub fn run(
    conn: &mut Connection,
    force: bool,
    max_age_hours: i64,
    json: bool,
) -> rusqlite::Result<()> {
    let resolvers = resolver::load_all();
    if resolvers.is_empty() {
        println!("No resolvers found (looked in $PLUGSCAN_RESOLVERS / --resolvers, ~/.config/plugscan/resolvers).");
        return Ok(());
    }
    let now = unix_now();
    let fresh_after = now - max_age_hours * 3600;

    let mut bundles: HashMap<(String, String), i64> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT v.name, p.name, p.id FROM bundles p JOIN vendors v ON v.id = p.vendor_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                (
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?.to_lowercase(),
                ),
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (k, v) = row?;
            bundles.insert(k, v);
        }
    }

    // Build the work list serially (needs the DB for the freshness check),
    // then fetch concurrently.
    let (mut missing, mut skipped, mut vendor_skips) = (0u32, 0u32, 0u32);
    let mut items: Vec<Work> = Vec::new();
    for rf in &resolvers {
        if rf.skip.is_some() {
            vendor_skips += 1;
            continue;
        }
        for pr in &rf.bundles {
            let key = (rf.vendor.clone(), pr.name.to_lowercase());
            let Some(&bundle_id) = bundles.get(&key) else {
                missing += 1;
                continue;
            };
            if !force {
                let fresh: bool = conn
                    .query_row(
                        "SELECT checked_at >= ?1 FROM checks WHERE bundle_id = ?2",
                        params![fresh_after, bundle_id],
                        |r| r.get(0),
                    )
                    .unwrap_or(false);
                if fresh {
                    skipped += 1;
                    continue;
                }
            }
            items.push(Work {
                vendor: &rf.vendor,
                name: &pr.name,
                via: pr.via.as_deref(),
                pr,
                bundle_id: Some(bundle_id),
            });
        }
    }

    let outcomes = run_concurrent(items);

    // Write results serially (rusqlite Connection is not Sync).
    let (mut checked, mut failed) = (0u32, 0u32);
    let tx = conn.transaction()?;
    for o in &outcomes {
        match &o.result {
            Ok(version) => {
                let action = outcomes_action_url(&resolvers, &o.vendor, &o.name, version);
                tx.execute(
                    "INSERT INTO checks(bundle_id, latest_version, url, source, checked_at)
                     VALUES(?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(bundle_id) DO UPDATE SET
                       latest_version=excluded.latest_version, url=excluded.url,
                       source=excluded.source, checked_at=excluded.checked_at",
                    params![
                        o.bundle_id,
                        version,
                        sanitize_line(&action),
                        match &o.via {
                            Some(via) => format!("resolver:{} via {via}", o.vendor),
                            None => format!("resolver:{}", o.vendor),
                        },
                        now
                    ],
                )?;
                checked += 1;
            }
            Err(_) => failed += 1,
        }
    }
    tx.commit()?;

    if json {
        emit_json(&outcomes);
        return Ok(());
    }
    for o in &outcomes {
        match &o.result {
            Ok(v) => match &o.via {
                Some(via) => println!("  {} {} → latest {v} [via {via}]", o.vendor, o.name),
                None => println!("  {} {} → latest {v}", o.vendor, o.name),
            },
            Err(reason) => eprintln!("warning: {} {}: {reason}", o.vendor, o.name),
        }
    }
    println!(
        "\nChecked {checked} bundles ({skipped} fresh, {missing} not installed, {failed} failed, {vendor_skips} vendors skip-listed)"
    );
    Ok(())
}

fn outcomes_action_url(
    resolvers: &[resolver::ResolverFile],
    vendor: &str,
    name: &str,
    version: &str,
) -> String {
    for rf in resolvers {
        if rf.vendor == vendor {
            for pr in &rf.bundles {
                if pr.name == name {
                    return action_url(pr, version);
                }
            }
        }
    }
    String::new()
}

fn emit_json(outcomes: &[Outcome]) {
    let rows: Vec<serde_json::Value> = outcomes
        .iter()
        .map(|o| {
            serde_json::json!({
                "vendor": o.vendor,
                "bundle": o.name,
                "via": o.via,
                "ok": o.result.is_ok(),
                "version": o.result.as_ref().ok(),
                "error": o.result.as_ref().err(),
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&rows).unwrap());
}

/// `resolver test`: exercise every resolver entry against the live web with
/// no catalog required — the CI nightly-exercise command. Reports which
/// extractions fail so resolver rot surfaces within a day. Exit-status via
/// the returned failure count.
pub fn test_all(json: bool, vendor_filter: Option<&str>) -> u32 {
    let resolvers = resolver::load_all();
    let mut items: Vec<Work> = Vec::new();
    for rf in &resolvers {
        if rf.skip.is_some() {
            continue;
        }
        if let Some(f) = vendor_filter {
            if !rf.vendor.to_lowercase().contains(&f.to_lowercase()) {
                continue;
            }
        }
        for pr in &rf.bundles {
            items.push(Work {
                vendor: &rf.vendor,
                name: &pr.name,
                via: pr.via.as_deref(),
                pr,
                bundle_id: None,
            });
        }
    }
    let total = items.len();
    let outcomes = run_concurrent(items);
    let failed: Vec<&Outcome> = outcomes.iter().filter(|o| o.result.is_err()).collect();

    if json {
        emit_json(&outcomes);
    } else {
        for o in &failed {
            println!(
                "FAIL  {} {}: {}",
                o.vendor,
                o.name,
                o.result.as_ref().err().unwrap()
            );
        }
        println!(
            "\nExercised {total} resolver entries: {} ok, {} FAILED",
            total - failed.len(),
            failed.len()
        );
    }
    failed.len() as u32
}

/// `resolver debug <vendor>`: run the vendor's resolver verbosely — the
/// brew-livecheck-style tool that makes contribution feasible.
pub fn debug_vendor(conn: &Connection, vendor: &str) -> rusqlite::Result<()> {
    let needle = vendor.to_lowercase();
    let mut fetcher = Fetcher::new();
    let mut found_any = false;
    for rf in resolver::load_all() {
        if !rf.vendor.to_lowercase().contains(&needle) {
            continue;
        }
        found_any = true;
        println!("vendor: {}", rf.vendor);
        if let Some(reason) = &rf.skip {
            println!("  skip-listed: {reason}");
        }
        for pr in &rf.bundles {
            let installed: Option<String> = conn
                .query_row(
                    "SELECT b.version FROM plugins b
                     JOIN bundles p ON p.id = b.bundle_id
                     JOIN vendors v ON v.id = p.vendor_id
                     WHERE v.name = ?1 AND p.name = ?2 COLLATE NOCASE
                       AND b.removed_at IS NULL LIMIT 1",
                    params![rf.vendor, pr.name],
                    |r| r.get(0),
                )
                .ok();
            println!("\n  [{}]", pr.name);
            println!(
                "    strategy:  {}",
                pr.strategy.as_deref().unwrap_or("page_regex")
            );
            println!("    page:      {}", pr.page);
            if let Some(re) = &pr.version_regex {
                println!("    regex:     {re}");
            }
            if pr.strategy.as_deref().unwrap_or("page_regex") == "page_regex" {
                if let (Some(body), Ok(Some(re))) = (
                    fetcher.page(&pr.page),
                    compile(&pr.version_regex, "version_regex"),
                ) {
                    let all: Vec<String> = re
                        .captures_iter(&body)
                        .filter_map(|c| c.get(1))
                        .map(|m| sanitize_line(m.as_str()))
                        .take(5)
                        .collect();
                    println!("    matches:   {}", all.join(", "));
                }
            }
            match resolve_version(&mut fetcher, pr) {
                Ok(v) => println!(
                    "    resolved:  {v}   (installed: {})",
                    installed.as_deref().unwrap_or("not installed")
                ),
                Err(e) => println!("    FAILED:    {e}"),
            }
        }
    }
    if !found_any {
        println!("No resolver file matches \"{vendor}\"");
    }
    Ok(())
}

/// `resolver new <vendor> --url <url>`: try strategies in cascade against a
/// URL and print a draft TOML.
pub fn new_vendor(vendor: &str, url: &str) {
    let mut fetcher = Fetcher::new();
    let slug = vendor
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    println!("# draft: save as resolvers/{slug}.toml after verifying");
    println!("vendor = \"{vendor}\"");
    println!("homepage = \"{url}\"\n");

    if url.contains("github.com") {
        let pr = BundleResolver {
            name: "PRODUCT".into(),
            page: url.into(),
            strategy: Some("github_release".into()),
            version_regex: None,
            json_path: None,
            exclude_regex: None,
            via: None,
            download: None,
            changelog: None,
        };
        match resolve_version(&mut fetcher, &pr) {
            Ok(v) => {
                println!("# github_release strategy works: latest = {v}");
                println!("[[bundle]]\nname = \"PRODUCT\"\npage = \"{url}\"\nstrategy = \"github_release\"");
                return;
            }
            Err(e) => println!("# github_release failed: {e}"),
        }
    }
    let Some(body) = fetcher.page(url) else {
        println!(
            "# fetch failed — site may block non-browser clients; write a manual-only resolver:"
        );
        println!("[manual]\nsteps = \"...\"");
        return;
    };
    if body.contains("sparkle:") {
        println!("# looks like a Sparkle appcast:");
        println!("[[bundle]]\nname = \"PRODUCT\"\npage = \"{url}\"\nstrategy = \"sparkle\"");
        return;
    }
    if serde_json::from_str::<serde_json::Value>(&body).is_ok() {
        println!("# JSON endpoint — pick a json_path:");
        println!("[[bundle]]\nname = \"PRODUCT\"\npage = \"{url}\"\nstrategy = \"json\"\njson_path = \"...\"");
        return;
    }
    let re = Regex::new(r"\b([0-9]+\.[0-9]+(?:\.[0-9]+)?)\b").unwrap();
    let mut seen = std::collections::BTreeSet::new();
    let hits: Vec<String> = re
        .captures_iter(&body)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .filter(|v| seen.insert(v.clone()))
        .take(8)
        .collect();
    println!(
        "# version-like strings on the page (in order): {}",
        hits.join(", ")
    );
    println!("# anchor a regex on nearby bundle text, first match must be latest:");
    println!("[[bundle]]\nname = \"PRODUCT\"\npage = \"{url}\"\nversion_regex = 'PRODUCT[^0-9]*([0-9]+\\.[0-9.]+)'");
}
