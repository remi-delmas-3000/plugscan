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
        // Read the body as bytes and decode lossily rather than read_to_string:
        // some resolver "pages" are binary sources whose text we scrape (the
        // Fractal ICONS release-notes PDF embeds "VERSION x.y.z" as plaintext
        // outline titles). read_to_string requires the *entire* body to be valid
        // UTF-8, so one non-UTF-8 byte anywhere aborts the read and surfaces as a
        // false "fetch failed". from_utf8_lossy keeps the plaintext we need and
        // makes the fetcher robust to PDFs and Latin-1 pages alike.
        let result = self
            .agent
            .get(url)
            .header("User-Agent", USER_AGENT)
            .call()
            .ok()
            .and_then(|mut res| {
                res.body_mut()
                    .with_config()
                    .limit(16 * 1024 * 1024)
                    .read_to_vec()
                    .ok()
            })
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
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

/// Classify why a resolve failed. Network errors are transient and worth a
/// retry (and must not fail CI); structural errors are real resolver rot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The fetch itself failed: DNS, TLS, timeout, connection reset.
    Network,
    /// The page loaded but extraction failed: regex/json_path did not match,
    /// bad config, prerelease-only. This is what "resolver rot" looks like.
    Structural,
}

#[derive(Debug, Clone)]
pub struct CheckError {
    pub kind: ErrorKind,
    pub message: String,
}
impl CheckError {
    fn net(m: impl Into<String>) -> Self {
        CheckError {
            kind: ErrorKind::Network,
            message: m.into(),
        }
    }
    fn structural(m: impl Into<String>) -> Self {
        CheckError {
            kind: ErrorKind::Structural,
            message: m.into(),
        }
    }
}
impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// What a strategy needs fetched, separated from extraction so the extraction
/// logic is testable offline against fixtures.
enum FetchPlan {
    Page(String),
    Headers(String),
}

fn fetch_plan(pr: &BundleResolver) -> FetchPlan {
    match pr.strategy.as_deref().unwrap_or("page_regex") {
        "header" => FetchPlan::Headers(pr.page.clone()),
        "github_release" => {
            let repo = pr
                .page
                .trim_start_matches("https://github.com/")
                .trim_end_matches('/');
            FetchPlan::Page(format!(
                "https://api.github.com/repos/{repo}/releases/latest"
            ))
        }
        _ => FetchPlan::Page(pr.page.clone()),
    }
}

/// Pure extraction: given the already-fetched text, apply the strategy. No
/// network, no I/O — this is the unit-tested core.
pub fn extract_version(pr: &BundleResolver, text: &str) -> Result<String, CheckError> {
    let exclude = Regex::new(pr.exclude_regex.as_deref().unwrap_or(DEFAULT_EXCLUDE))
        .map_err(|e| CheckError::structural(format!("bad exclude_regex: {e}")))?;
    let refine = compile(&pr.version_regex, "version_regex").map_err(CheckError::structural)?;
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
            let re = refine
                .as_ref()
                .ok_or_else(|| CheckError::structural("page_regex requires version_regex"))?;
            re.captures_iter(text)
                .filter_map(|c| c.get(1))
                .map(|m| m.as_str().to_string())
                .find(|v| !exclude.is_match(v))
                .ok_or_else(|| CheckError::structural("no non-prerelease match on page"))?
        }
        "json" => {
            let doc: serde_json::Value = serde_json::from_str(text)
                .map_err(|e| CheckError::structural(format!("invalid JSON: {e}")))?;
            let path = pr
                .json_path
                .as_deref()
                .ok_or_else(|| CheckError::structural("json requires json_path"))?;
            let v =
                json_at(&doc, path).ok_or_else(|| CheckError::structural("json_path not found"))?;
            let raw = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            refine_or_pass(&raw)
                .ok_or_else(|| CheckError::structural("version_regex did not match json value"))?
        }
        "github_release" => {
            let doc: serde_json::Value = serde_json::from_str(text)
                .map_err(|e| CheckError::structural(format!("invalid JSON: {e}")))?;
            if doc.get("prerelease").and_then(|v| v.as_bool()) == Some(true) {
                return Err(CheckError::structural("latest release is a prerelease"));
            }
            let tag = doc
                .get("tag_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| CheckError::structural("no tag_name"))?;
            let stripped = tag.trim_start_matches(['v', 'V']).to_string();
            refine_or_pass(&stripped)
                .ok_or_else(|| CheckError::structural("version_regex did not match tag"))?
        }
        "sparkle" => {
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
                    .captures_iter(text)
                    .filter_map(|c| c.get(1))
                    .map(|m| m.as_str().to_string())
                    .find(|v| !exclude.is_match(v));
                if hit.is_some() {
                    found = hit;
                    break;
                }
            }
            found.ok_or_else(|| CheckError::structural("no version in appcast"))?
        }
        "header" => {
            let re = refine
                .as_ref()
                .ok_or_else(|| CheckError::structural("header requires version_regex"))?;
            re.captures(text)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
                .ok_or_else(|| CheckError::structural("version not in redirect headers"))?
        }
        other => return Err(CheckError::structural(format!("unknown strategy: {other}"))),
    };

    let version = sanitize_line(&version);
    if exclude.is_match(&version) {
        return Err(CheckError::structural(format!(
            "matched prerelease: {version}"
        )));
    }
    if version.is_empty() {
        return Err(CheckError::structural("empty version"));
    }
    Ok(version)
}

/// Fetch (network) then extract (pure). Fetch failures are Network errors.
pub fn resolve_version_kind(f: &mut Fetcher, pr: &BundleResolver) -> Result<String, CheckError> {
    let text = match fetch_plan(pr) {
        FetchPlan::Page(url) => f
            .page(&url)
            .ok_or_else(|| CheckError::net("fetch failed"))?,
        FetchPlan::Headers(url) => f
            .headers(&url)
            .ok_or_else(|| CheckError::net("request failed"))?,
    };
    extract_version(pr, &text)
}

/// String-only wrapper kept for the debug/new tools.
pub fn resolve_version(f: &mut Fetcher, pr: &BundleResolver) -> Result<String, String> {
    resolve_version_kind(f, pr).map_err(|e| e.message)
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
    pub paid_from: Option<&'a str>,
    pub pr: &'a BundleResolver,
    pub bundle_id: Option<i64>,
}

pub struct Outcome {
    pub vendor: String,
    pub name: String,
    pub via: Option<String>,
    pub paid_from: Option<String>,
    pub bundle_id: Option<i64>,
    pub result: Result<String, CheckError>,
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
                    // Retry network failures (transient); structural failures
                    // (real rot) fail immediately — retrying a broken regex is
                    // pointless and only delays the run.
                    let mut result = resolve_version_kind(&mut f, w.pr);
                    let mut tries = 0;
                    while let Err(e) = &result {
                        if e.kind != ErrorKind::Network || tries >= 2 {
                            break;
                        }
                        tries += 1;
                        std::thread::sleep(std::time::Duration::from_millis(500 * tries));
                        result = resolve_version_kind(&mut f, w.pr);
                    }
                    out.push(Outcome {
                        vendor: w.vendor.to_string(),
                        name: w.name.to_string(),
                        via: w.via.map(str::to_string),
                        paid_from: w.paid_from.map(str::to_string),
                        bundle_id: w.bundle_id,
                        result,
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
    vendor_filter: Option<&str>,
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

    // Highest installed version per bundle, so a resolved "latest" can be
    // sanity-checked against what's actually on disk: a result below installed
    // is never an update and signals a misaimed resolver (see report::below_installed).
    let mut installed: HashMap<i64, String> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT bundle_id, version FROM plugins
             WHERE removed_at IS NULL AND version IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, v) = row?;
            installed
                .entry(id)
                .and_modify(|cur| {
                    if crate::report::cmp_versions(&v, cur) == std::cmp::Ordering::Greater {
                        *cur = v.clone();
                    }
                })
                .or_insert(v);
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
        if let Some(vf) = vendor_filter {
            if !rf.vendor.to_lowercase().contains(&vf.to_lowercase()) {
                continue;
            }
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
                paid_from: pr.paid_from.as_deref(),
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
                    "INSERT INTO checks(bundle_id, latest_version, url, source, checked_at, paid_from)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(bundle_id) DO UPDATE SET
                       latest_version=excluded.latest_version, url=excluded.url,
                       source=excluded.source, checked_at=excluded.checked_at,
                       paid_from=excluded.paid_from",
                    params![
                        o.bundle_id,
                        version,
                        sanitize_line(&action),
                        match &o.via {
                            Some(via) => format!("resolver:{} via {via}", o.vendor),
                            None => format!("resolver:{}", o.vendor),
                        },
                        now,
                        o.paid_from,
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
    let mut below = 0u32;
    for o in &outcomes {
        match &o.result {
            Ok(v) => {
                // A resolved version below what's installed is never an update;
                // flag it so a misaimed resolver surfaces the moment it's run,
                // not only buried in `outdated`'s RESOLVER STALE section.
                let inst = o.bundle_id.and_then(|id| installed.get(&id));
                let warn = match inst {
                    Some(i) if crate::report::below_installed(v, i) => {
                        below += 1;
                        format!("  ⚠ below installed {i} — resolver likely misaimed")
                    }
                    _ => String::new(),
                };
                match &o.via {
                    Some(via) => {
                        println!("  {} {} → latest {v} [via {via}]{warn}", o.vendor, o.name)
                    }
                    None => println!("  {} {} → latest {v}{warn}", o.vendor, o.name),
                }
            }
            Err(e) => eprintln!("warning: {} {}: {}", o.vendor, o.name, e.message),
        }
    }
    println!(
        "\nChecked {checked} bundles ({skipped} fresh, {missing} not installed, {failed} failed, {vendor_skips} vendors skip-listed)"
    );
    if below > 0 {
        println!(
            "  ⚠ {below} resolved below the installed version (likely misaimed resolvers — see `outdated` → RESOLVER STALE)"
        );
    }
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
                "error": o.result.as_ref().err().map(|e| e.message.clone()),
                "error_kind": o.result.as_ref().err().map(|e| match e.kind {
                    ErrorKind::Network => "network",
                    ErrorKind::Structural => "structural",
                }),
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
                paid_from: pr.paid_from.as_deref(),
                pr,
                bundle_id: None,
            });
        }
    }
    let total = items.len();
    let outcomes = run_concurrent(items);
    let structural: Vec<&Outcome> = outcomes
        .iter()
        .filter(|o| matches!(&o.result, Err(e) if e.kind == ErrorKind::Structural))
        .collect();
    let network: Vec<&Outcome> = outcomes
        .iter()
        .filter(|o| matches!(&o.result, Err(e) if e.kind == ErrorKind::Network))
        .collect();

    if json {
        emit_json(&outcomes);
    } else {
        for o in &structural {
            println!(
                "ROT   {} {}: {}",
                o.vendor,
                o.name,
                o.result.as_ref().err().unwrap()
            );
        }
        for o in &network {
            println!(
                "net?  {} {}: {} (transient, not a failure)",
                o.vendor,
                o.name,
                o.result.as_ref().err().unwrap()
            );
        }
        println!(
            "\nExercised {total} resolver entries: {} ok, {} rot, {} transient network",
            total - structural.len() - network.len(),
            structural.len(),
            network.len()
        );
    }
    // Only structural failures (real rot) fail CI; network blips do not.
    structural.len() as u32
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
            paid_from: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::BundleResolver;

    fn r(strategy: Option<&str>, regex: Option<&str>, json_path: Option<&str>) -> BundleResolver {
        BundleResolver {
            name: "X".into(),
            page: "https://example.com".into(),
            strategy: strategy.map(str::to_string),
            version_regex: regex.map(str::to_string),
            json_path: json_path.map(str::to_string),
            exclude_regex: None,
            via: None,
            paid_from: None,
            download: None,
            changelog: None,
        }
    }

    // --- page_regex ---
    #[test]
    fn page_regex_first_match_is_latest() {
        // vendor changelog lists newest first
        let body = "<h2>Pro-Q 4.13</h2> ... <h2>Pro-Q 4.10</h2>";
        let pr = r(None, Some(r"Pro-Q ([0-9]+\.[0-9]+)"), None);
        assert_eq!(extract_version(&pr, body).unwrap(), "4.13");
    }

    #[test]
    fn page_regex_skips_prerelease_to_stable() {
        let body = "Latest: 2.0.0-beta then stable 1.9.5";
        let pr = r(None, Some(r"([0-9]+\.[0-9]+\.[0-9]+(?:-beta)?)"), None);
        // default exclude drops the beta, takes the next
        assert_eq!(extract_version(&pr, body).unwrap(), "1.9.5");
    }

    #[test]
    fn page_regex_ignores_theme_asset_when_anchored() {
        // the real Eventide/McDSP trap: ?ver= asset tokens must not win
        let body = "styles.css?ver=6.12.0 ... Blackhole Installer (Mac 64-bit) Version 3.11.4";
        let pr = r(
            None,
            Some(r"Mac[^)]*\)[^0-9]*Version ([0-9]+\.[0-9.]+)"),
            None,
        );
        assert_eq!(extract_version(&pr, body).unwrap(), "3.11.4");
    }

    #[test]
    fn page_regex_no_match_is_structural() {
        let pr = r(None, Some(r"Version ([0-9.]+)"), None);
        let e = extract_version(&pr, "no versions here").unwrap_err();
        assert_eq!(e.kind, ErrorKind::Structural);
    }

    #[test]
    fn page_regex_missing_regex_is_structural() {
        let pr = r(None, None, None);
        assert_eq!(
            extract_version(&pr, "x").unwrap_err().kind,
            ErrorKind::Structural
        );
    }

    // --- json ---
    #[test]
    fn json_dotpath_and_array_index() {
        let body = r#"{"results":[{"version":"1.7.13"},{"version":"1.0.0"}]}"#;
        let pr = r(Some("json"), None, Some("results.0.version"));
        assert_eq!(extract_version(&pr, body).unwrap(), "1.7.13");
    }

    #[test]
    fn json_numeric_value_stringifies() {
        let body = r#"{"v": 26}"#;
        let pr = r(Some("json"), None, Some("v"));
        assert_eq!(extract_version(&pr, body).unwrap(), "26");
    }

    #[test]
    fn json_path_missing_is_structural() {
        let pr = r(Some("json"), None, Some("nope.here"));
        assert_eq!(
            extract_version(&pr, "{}").unwrap_err().kind,
            ErrorKind::Structural
        );
    }

    #[test]
    fn json_invalid_is_structural() {
        let pr = r(Some("json"), None, Some("v"));
        assert_eq!(
            extract_version(&pr, "<html>").unwrap_err().kind,
            ErrorKind::Structural
        );
    }

    #[test]
    fn json_refine_regex_on_value() {
        let body = r#"{"tag":"release-3.4.6-final"}"#;
        let pr = r(Some("json"), Some(r"([0-9]+\.[0-9]+\.[0-9]+)"), Some("tag"));
        assert_eq!(extract_version(&pr, body).unwrap(), "3.4.6");
    }

    // --- github_release ---
    #[test]
    fn github_strips_v_prefix() {
        let body = r#"{"tag_name":"v1.0.3","prerelease":false}"#;
        let pr = r(Some("github_release"), None, None);
        assert_eq!(extract_version(&pr, body).unwrap(), "1.0.3");
    }

    #[test]
    fn github_rejects_prerelease() {
        let body = r#"{"tag_name":"v2.0.0","prerelease":true}"#;
        let pr = r(Some("github_release"), None, None);
        assert_eq!(
            extract_version(&pr, body).unwrap_err().kind,
            ErrorKind::Structural
        );
    }

    // --- sparkle ---
    #[test]
    fn sparkle_short_version_attr() {
        let body = r#"<item><enclosure sparkle:shortVersionString="2.3.1" sparkle:version="2301"/></item>"#;
        let pr = r(Some("sparkle"), None, None);
        assert_eq!(extract_version(&pr, body).unwrap(), "2.3.1");
    }

    #[test]
    fn sparkle_element_form() {
        let body = "<sparkle:shortVersionString>1.4.9</sparkle:shortVersionString>";
        let pr = r(Some("sparkle"), None, None);
        assert_eq!(extract_version(&pr, body).unwrap(), "1.4.9");
    }

    // --- header ---
    #[test]
    fn header_extracts_from_redirect() {
        let joined =
            "https://cdn.example.com/Thing_v1.2.3_mac.dmg\nattachment; filename=Thing_v1.2.3.dmg\n";
        let pr = r(Some("header"), Some(r"_v([0-9]+\.[0-9]+\.[0-9]+)_"), None);
        assert_eq!(extract_version(&pr, joined).unwrap(), "1.2.3");
    }

    // --- misc ---
    #[test]
    fn unknown_strategy_is_structural() {
        let pr = r(Some("telepathy"), None, None);
        assert_eq!(
            extract_version(&pr, "x").unwrap_err().kind,
            ErrorKind::Structural
        );
    }

    #[test]
    fn control_chars_stripped_from_version() {
        let pr = r(None, Some(r"v(.+)"), None);
        // a version capture with an embedded escape is sanitized
        let got = extract_version(&pr, "v1.2\x1b[31m.3").unwrap();
        assert!(!got.contains('\x1b'));
    }

    use proptest::prelude::*;

    proptest! {
        // The extraction engine must NEVER panic, whatever a live page throws
        // at it: arbitrary bytes, every strategy, arbitrary user regex. It may
        // only ever return Ok or Err.
        #[test]
        fn extract_never_panics(
            text in ".{0,600}",
            strat in prop::option::of(prop::sample::select(vec![
                "page_regex", "json", "github_release", "sparkle", "header", "bogus",
            ])),
            rx in prop::option::of("[A-Za-z0-9().*+?\\ -]{0,20}"),
            jp in prop::option::of("[a-z0-9.]{0,20}"),
        ) {
            let pr = r(strat, rx.as_deref(), jp.as_deref());
            let _ = extract_version(&pr, &text);
        }

        // A user-supplied version_regex that fails to compile is a structural
        // error, never a panic.
        #[test]
        fn bad_user_regex_is_structural_not_panic(rx in ".{0,30}") {
            let pr = r(None, Some(&rx), None);
            if let Err(e) = extract_version(&pr, "some 1.2.3 text") {
                // compile failures and no-match are both structural
                prop_assert_eq!(e.kind, ErrorKind::Structural);
            }
        }

        // Any successfully extracted version is free of control characters
        // (the terminal-injection guard holds for all inputs).
        #[test]
        fn extracted_version_has_no_control_chars(
            text in ".{0,400}",
            rx in "[A-Za-z0-9().*+? -]{1,20}",
        ) {
            let pr = r(None, Some(&rx), None);
            if let Ok(v) = extract_version(&pr, &text) {
                prop_assert!(!v.chars().any(|c| c.is_control()));
            }
        }

        // JSON strategy on arbitrary text never panics and only errs structurally.
        #[test]
        fn json_strategy_robust(text in ".{0,400}", path in "[a-z0-9.]{0,20}") {
            let pr = r(Some("json"), None, Some(&path));
            if let Err(e) = extract_version(&pr, &text) {
                prop_assert_eq!(e.kind, ErrorKind::Structural);
            }
        }
    }
}
