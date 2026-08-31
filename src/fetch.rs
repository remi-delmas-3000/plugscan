use crate::db::unix_now;
use crate::report::{cmp_versions, cmp_versions_prefix};
use crate::util::sanitize_line;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/128.0 Safari/537.36 plugscan/",
    env!("CARGO_PKG_VERSION")
);
const MAX_BYTES: u64 = 3 * 1024 * 1024 * 1024; // 3 GiB cap
const MAX_HOPS: usize = 5;

fn archive_root() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME not set");
    PathBuf::from(home).join("Library/Application Support/plugscan/archive")
}

fn slug(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_string()
}

/// Filename for the saved installer: Content-Disposition first, then the URL
/// path. Sanitized hard — this becomes a filesystem path component.
fn pick_filename(disposition: Option<&str>, url: &str) -> String {
    let from_disposition = disposition.and_then(|d| {
        regex::Regex::new(r#"filename\*?=(?:UTF-8''|")?([^";]+)"#)
            .unwrap()
            .captures(d)
            .map(|c| c[1].to_string())
    });
    let raw = from_disposition.unwrap_or_else(|| {
        url.split('?')
            .next()
            .unwrap_or("")
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string()
    });
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || " ._-".contains(*c))
        .collect();
    let cleaned = cleaned.trim_matches(['.', ' ']).to_string();
    if cleaned.is_empty() {
        "installer.bin".to_string()
    } else {
        cleaned
    }
}

fn agent() -> ureq::Agent {
    // Redirects are followed manually so every hop can be verified https.
    ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(15)))
        .timeout_recv_response(Some(std::time::Duration::from_secs(30)))
        .timeout_global(Some(std::time::Duration::from_secs(600)))
        .max_redirects(0)
        .http_status_as_error(false)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .build(),
        )
        .build()
        .into()
}

fn host_of(url: &str) -> Option<&str> {
    url.strip_prefix("https://")?.split(['/', '?']).next()
}

/// GET with manual redirect following; every hop must be https.
fn get_verified(
    agent: &ureq::Agent,
    start_url: &str,
) -> Result<(ureq::http::Response<ureq::Body>, String), String> {
    let mut url = start_url.to_string();
    for _ in 0..=MAX_HOPS {
        if !url.starts_with("https://") {
            return Err(format!("redirect left https: {}", sanitize_line(&url)));
        }
        let res = agent
            .get(&url)
            .header("User-Agent", USER_AGENT)
            .call()
            .map_err(|e| format!("request failed: {e}"))?;
        let status = res.status().as_u16();
        if (301..=308).contains(&status) {
            let location = res
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or("redirect without Location")?
                .to_string();
            url = if location.starts_with("https://") || location.starts_with("http://") {
                location
            } else if location.starts_with('/') {
                let host = host_of(&url).ok_or("bad url")?;
                format!("https://{host}{location}")
            } else {
                return Err("unsupported relative redirect".into());
            };
            continue;
        }
        if status != 200 {
            return Err(format!("HTTP {status}"));
        }
        return Ok((res, url));
    }
    Err("too many redirects".into())
}

enum Probe {
    File(PathBuf, String, u64), // path, sha256, bytes
    Page,
}

fn download_if_file(agent: &ureq::Agent, url: &str, dest_dir: &Path) -> Result<Probe, String> {
    let (mut res, final_url) = get_verified(agent, url)?;
    let content_type = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    let disposition = res
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let is_attachment = disposition
        .as_deref()
        .map(|d| d.to_lowercase().contains("attachment"))
        .unwrap_or(false);
    let is_page =
        content_type.starts_with("text/html") || content_type.starts_with("application/xhtml");
    if is_page && !is_attachment {
        return Ok(Probe::Page);
    }

    let filename = pick_filename(disposition.as_deref(), &final_url);
    std::fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
    let dest = dest_dir.join(&filename);
    let tmp = dest_dir.join(format!(".{filename}.part"));

    let mut reader = res.body_mut().as_reader();
    let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > MAX_BYTES {
            let _ = std::fs::remove_file(&tmp);
            return Err("exceeds 3 GiB cap".into());
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;

    // Gatekeeper stays in the loop: quarantine the file exactly as a browser
    // download would be.
    let stamp = format!("0083;{:x};plugscan;", unix_now());
    let _ = Command::new("xattr")
        .args(["-w", "com.apple.quarantine", &stamp])
        .arg(&dest)
        .status();

    let sha = format!("{:x}", hasher.finalize());
    Ok(Probe::File(dest, sha, total))
}

struct Target {
    bundle_id: i64,
    vendor: String,
    bundle: String,
    installed: String,
    latest: String,
    url: String,
}

fn stale_targets(conn: &Connection, filter: Option<&str>) -> rusqlite::Result<Vec<Target>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, v.name, p.name,
                group_concat(DISTINCT COALESCE(b.version,'?')),
                c.latest_version, c.url
         FROM bundles p
         JOIN vendors v ON v.id = p.vendor_id
         JOIN plugins b ON b.bundle_id = p.id AND b.removed_at IS NULL
         JOIN checks c ON c.bundle_id = p.id
         LEFT JOIN user_meta um ON um.bundle_id = p.id
         WHERE c.latest_version IS NOT NULL
           AND COALESCE(um.ignored, 0) = 0
           AND (?1 IS NULL OR p.name LIKE '%' || ?1 || '%')
         GROUP BY p.id",
    )?;
    let rows = stmt.query_map(params![filter], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            r.get::<_, String>(4)?,
            r.get::<_, Option<String>>(5)?.unwrap_or_default(),
        ))
    })?;
    let mut targets = Vec::new();
    for row in rows {
        let (bundle_id, vendor, bundle, versions, latest, url) = row?;
        let installed = versions
            .split(',')
            .max_by(|a, b| cmp_versions(a, b))
            .unwrap_or("?")
            .to_string();
        if cmp_versions_prefix(&latest, &installed) == Ordering::Greater && !url.is_empty() {
            targets.push(Target {
                bundle_id,
                vendor,
                bundle,
                installed,
                latest,
                url,
            });
        }
    }
    Ok(targets)
}

pub fn run(
    conn: &mut Connection,
    filter: Option<&str>,
    open_pages: bool,
    out: Option<&Path>,
) -> rusqlite::Result<()> {
    let targets = stale_targets(conn, filter)?;
    if targets.is_empty() {
        println!("Nothing stale to fetch.");
        return Ok(());
    }
    // Vendors that ship their own download manager: recommend it, don't probe.
    let download_via: std::collections::HashMap<String, String> = crate::resolver::load_all()
        .into_iter()
        .filter_map(|rf| rf.download_via.map(|v| (rf.vendor, v)))
        .collect();
    let agent = agent();
    let now = unix_now();
    let (mut saved, mut pages, mut skipped, mut failed) = (0u32, Vec::new(), 0u32, 0u32);
    let mut via_counts: std::collections::BTreeMap<&str, (u32, &str)> = Default::default();
    // Stop hammering a host after repeated failures (Melda-style 403 walls).
    let mut host_fails: std::collections::HashMap<String, u32> = Default::default();

    for t in &targets {
        if let Some(via) = download_via.get(&t.vendor) {
            via_counts.entry(t.vendor.as_str()).or_insert((0, via)).0 += 1;
            continue;
        }
        if let Some(host) = host_of(&t.url) {
            if host_fails.get(host).copied().unwrap_or(0) >= 3 {
                failed += 1;
                continue;
            }
        }
        // --out is a one-off grab into a user directory: no archive
        // bookkeeping, no skip-if-archived.
        if let Some(out_dir) = out {
            match download_if_file(&agent, &t.url, out_dir) {
                Ok(Probe::File(path, _, bytes)) => {
                    println!("  saved  {} ({:.1} MB)", path.display(), bytes as f64 / 1e6);
                    saved += 1;
                }
                Ok(Probe::Page) => pages.push(t),
                Err(e) => {
                    eprintln!("warning: {} {}: {e}", t.vendor, t.bundle);
                    failed += 1;
                }
            }
            continue;
        }
        let already: Option<String> = conn
            .query_row(
                "SELECT path FROM downloads WHERE bundle_id = ?1 AND version = ?2",
                params![t.bundle_id, t.latest],
                |r| r.get(0),
            )
            .ok();
        if let Some(path) = already {
            if Path::new(&path).exists() {
                skipped += 1;
                continue;
            }
        }
        println!(
            "  probing {} {} → {}",
            t.vendor,
            t.bundle,
            sanitize_line(&t.url)
        );
        let dest_dir = archive_root()
            .join(slug(&t.vendor))
            .join(slug(&t.bundle))
            .join(slug(&t.latest));
        match download_if_file(&agent, &t.url, &dest_dir) {
            Ok(Probe::File(path, sha, bytes)) => {
                conn.execute(
                    "INSERT INTO downloads(bundle_id, version, url, path, sha256, bytes, fetched_at)
                     VALUES(?1,?2,?3,?4,?5,?6,?7)
                     ON CONFLICT(bundle_id, version) DO UPDATE SET url=excluded.url,
                       path=excluded.path, sha256=excluded.sha256, bytes=excluded.bytes,
                       fetched_at=excluded.fetched_at",
                    params![t.bundle_id, t.latest, t.url, path.display().to_string(), sha, bytes as i64, now],
                )?;
                println!(
                    "  saved  {} {} {} ({:.1} MB)",
                    t.vendor,
                    t.bundle,
                    t.latest,
                    bytes as f64 / 1e6
                );
                saved += 1;
            }
            Ok(Probe::Page) => pages.push(t),
            Err(e) => {
                eprintln!("warning: {} {}: {e}", t.vendor, t.bundle);
                failed += 1;
                if let Some(host) = host_of(&t.url) {
                    let n = host_fails.entry(host.to_string()).or_insert(0);
                    *n += 1;
                    if *n == 3 {
                        eprintln!("warning: {host}: repeated failures, skipping remaining URLs on this host");
                    }
                }
            }
        }
    }

    if !via_counts.is_empty() {
        println!("\nVENDOR DOWNLOAD MANAGERS (updates ship through the vendor's own app)");
        for (vendor, (count, via)) in &via_counts {
            println!("  {vendor}: {count} updates → {via}");
        }
    }
    if !pages.is_empty() {
        println!("\nDOWNLOAD PAGES (not direct files — open in your logged-in browser)");
        for t in &pages {
            println!(
                "  {} {}  {} → {}   {}",
                t.vendor,
                t.bundle,
                t.installed,
                t.latest,
                sanitize_line(&t.url)
            );
        }
        if open_pages {
            for t in &pages {
                let _ = Command::new("open").arg(&t.url).status();
            }
            println!("  (opened {} pages in your browser)", pages.len());
        }
    }
    println!(
        "\nFetched {saved} installers into {} ({} pages, {skipped} already archived, {failed} failed)",
        out.map(|p| p.display().to_string()).unwrap_or_else(|| archive_root().display().to_string()),
        pages.len()
    );
    Ok(())
}

/// Delete archived installers (optionally one bundle) and their records.
pub fn clear(conn: &mut Connection, bundle: Option<&str>) -> rusqlite::Result<()> {
    let rows: Vec<(i64, String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT d.id, d.path, COALESCE(d.bytes, 0)
             FROM downloads d JOIN bundles p ON p.id = d.bundle_id
             WHERE (?1 IS NULL OR p.name LIKE '%' || ?1 || '%')",
        )?;
        let mapped = stmt.query_map(params![bundle], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        mapped.collect::<Result<_, _>>()?
    };
    if rows.is_empty() {
        println!(
            "Nothing archived{}.",
            bundle
                .map(|p| format!(" matching \"{p}\""))
                .unwrap_or_default()
        );
        return Ok(());
    }
    let mut freed: i64 = 0;
    for (id, path, bytes) in &rows {
        let p = Path::new(path);
        if p.exists() {
            let _ = std::fs::remove_file(p);
            freed += bytes;
        }
        // Prune now-empty version/bundle/vendor dirs up to the archive root.
        let mut dir = p.parent();
        while let Some(d) = dir {
            if !d.starts_with(archive_root()) || d == archive_root() {
                break;
            }
            if std::fs::remove_dir(d).is_err() {
                break; // not empty
            }
            dir = d.parent();
        }
        conn.execute("DELETE FROM downloads WHERE id = ?1", [id])?;
    }
    println!(
        "Cleared {} archived installers, freed {:.1} MB",
        rows.len(),
        freed as f64 / 1e6
    );
    Ok(())
}

/// Import a manually-downloaded installer (gated vendors) into the archive.
pub fn import(
    conn: &mut Connection,
    file: &Path,
    bundle: &str,
    version: Option<&str>,
) -> rusqlite::Result<()> {
    let matches: Vec<(i64, String, String, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT p.id, v.name, p.name, c.latest_version
             FROM bundles p JOIN vendors v ON v.id = p.vendor_id
             LEFT JOIN checks c ON c.bundle_id = p.id
             WHERE p.name LIKE '%' || ?1 || '%'",
        )?;
        let rows = stmt.query_map([bundle], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        rows.collect::<Result<_, _>>()?
    };
    match matches.len() {
        0 => {
            println!("No bundle matching \"{bundle}\"");
            return Ok(());
        }
        1 => {}
        n => {
            println!("Ambiguous — {n} bundles match, be more specific:");
            for (_, v, p, _) in matches.iter().take(20) {
                println!("  {v} — {p}");
            }
            return Ok(());
        }
    }
    let (bundle_id, vendor, product_name, latest) = matches.into_iter().next().unwrap();
    let Some(version) = version.map(str::to_string).or(latest) else {
        println!("No known latest version — pass --version");
        return Ok(());
    };

    let data =
        std::fs::read(file).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let sha = format!("{:x}", Sha256::digest(&data));
    let bytes = data.len() as i64;
    let filename = file
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("installer.bin");
    let dest_dir = archive_root()
        .join(slug(&vendor))
        .join(slug(&product_name))
        .join(slug(&version));
    std::fs::create_dir_all(&dest_dir)
        .and_then(|_| std::fs::copy(file, dest_dir.join(filename)).map(|_| ()))
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let dest = dest_dir.join(filename);
    conn.execute(
        "INSERT INTO downloads(bundle_id, version, url, path, sha256, bytes, fetched_at)
         VALUES(?1,?2,NULL,?3,?4,?5,?6)
         ON CONFLICT(bundle_id, version) DO UPDATE SET path=excluded.path,
           sha256=excluded.sha256, bytes=excluded.bytes, fetched_at=excluded.fetched_at",
        params![
            bundle_id,
            version,
            dest.display().to_string(),
            sha,
            bytes,
            unix_now()
        ],
    )?;
    println!(
        "archived {vendor} — {product_name} {version} → {}",
        dest.display()
    );
    Ok(())
}
