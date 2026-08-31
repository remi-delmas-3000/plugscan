use crate::db::unix_now;
use crate::vendors;
use plist::Value;
use rayon::prelude::*;
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

pub struct ScannedBundle {
    pub path: String,
    pub format: &'static str,
    pub name: String,
    pub version: Option<String>,
    pub bundle_id: Option<String>,
    pub vendor: String,
    pub mtime: i64,
}

fn format_for_ext(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "component" => Some("AU"),
        "vst3" => Some("VST3"),
        "vst" => Some("VST2"),
        "clap" => Some("CLAP"),
        _ => None,
    }
}

/// Waves plugins are individual .bundle folders under /Applications/Waves/
/// Plug-Ins V<N>/, loaded at runtime by the WaveShell host. Enumerate the
/// newest installed version directory only: older V<N> dirs are kept for
/// session compatibility and would otherwise register as stale duplicates.
/// Each .bundle carries a normal CFBundleShortVersionString.
fn find_waves_plugins() -> Vec<(PathBuf, &'static str)> {
    let base = Path::new("/Applications/Waves");
    let Ok(entries) = fs::read_dir(base) else {
        return Vec::new();
    };
    let newest = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_prefix("Plug-Ins V")
                .and_then(|n| n.parse::<u32>().ok())
                .map(|v| (v, e.path()))
        })
        .max_by_key(|(v, _)| *v);
    let Some((_, dir)) = newest else {
        return Vec::new();
    };
    let Ok(bundles) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    bundles
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("bundle"))
        .map(|p| (p, "Waves"))
        .collect()
}

fn read_waves_plugin(path: &Path, mtime: i64) -> ScannedBundle {
    let name = crate::util::sanitize_line(path.file_stem().and_then(|s| s.to_str()).unwrap_or("?"));
    let get = |v: &Value, key: &str| {
        v.as_dictionary()
            .and_then(|d| d.get(key))
            .and_then(|x| x.as_string())
            .map(str::to_string)
    };
    let (version, bundle_id) = match Value::from_file(path.join("Contents/Info.plist")) {
        Ok(v) => (
            get(&v, "CFBundleShortVersionString").map(|s| crate::util::sanitize_line(&s)),
            get(&v, "CFBundleIdentifier").map(|s| crate::util::sanitize_line(&s)),
        ),
        Err(_) => (None, None),
    };
    ScannedBundle {
        path: path.display().to_string(),
        format: "Waves",
        name,
        version,
        bundle_id,
        vendor: "Waves".to_string(),
        mtime,
    }
}

fn search_roots() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let bases = [
        "/Library/Audio/Plug-Ins".to_string(),
        format!("{home}/Library/Audio/Plug-Ins"),
    ];
    let mut roots = Vec::new();
    for base in &bases {
        for sub in ["Components", "VST3", "VST", "CLAP"] {
            roots.push(Path::new(base).join(sub));
        }
    }
    roots
}

/// DAWs tracked as applications: (name prefix of the .app, vendor, product).
/// Version comes from the app bundle's Info.plist like any other bundle.
const DAW_APPS: &[(&str, &str, &str)] = &[
    ("REAPER", "Cockos", "REAPER"),
    ("Bitwig Studio", "Bitwig", "Bitwig Studio"),
    ("Cubase", "Steinberg", "Cubase"),
    ("Fender Studio", "Fender", "Fender Studio"),
    ("Renoise", "Renoise", "Renoise"),
    ("Reason", "Reason Studios", "Reason"),
    ("FL Studio", "Image-Line", "FL Studio"),
    ("LUNA", "Universal Audio", "LUNA"),
    ("Ableton Live", "Ableton", "Ableton Live"),
    ("Logic Pro", "Apple", "Logic Pro"),
];

fn find_daw_apps() -> Vec<(PathBuf, &'static str, &'static str)> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut out = Vec::new();
    for dir in ["/Applications".to_string(), format!("{home}/Applications")] {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("app") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Some((_, vendor, product)) = DAW_APPS
                .iter()
                .find(|(pat, _, _)| stem.to_lowercase().starts_with(&pat.to_lowercase()))
            {
                out.push((path, *vendor, *product));
            }
        }
    }
    out
}

fn read_app(path: &Path, vendor: &str, product: &str, mtime: i64) -> ScannedBundle {
    let version = Value::from_file(path.join("Contents/Info.plist"))
        .ok()
        .and_then(|v| {
            let d = v.as_dictionary()?;
            let get = |k: &str| d.get(k).and_then(|v| v.as_string()).map(str::to_string);
            get("CFBundleShortVersionString").or_else(|| get("CFBundleVersion"))
        })
        .map(|s| normalize_version(crate::util::sanitize_line(&s)));
    ScannedBundle {
        path: path.display().to_string(),
        format: "APP",
        name: product.to_string(),
        version,
        bundle_id: None,
        vendor: vendor.to_string(),
        mtime,
    }
}

/// Find plugin bundles under `root`, recursing into plain subfolders
/// (some vendors nest VST3s) but never into bundles themselves.
fn find_bundles(root: &Path, depth: u32, out: &mut Vec<(PathBuf, &'static str)>) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let fmt = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(format_for_ext);
        let is_waveshell = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("WaveShell"))
            .unwrap_or(false);
        if is_waveshell {
            continue;
        }
        if let Some(fmt) = fmt {
            out.push((path, fmt));
        } else if path.is_dir() {
            find_bundles(&path, depth + 1, out);
        }
    }
}

fn mtime_of(path: &Path) -> i64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn vendor_from(bundle_id: &str, plist: &Value) -> String {
    // AU bundles usually carry "Vendor: Plugin" in AudioComponents[0].name.
    if let Some(name) = plist
        .as_dictionary()
        .and_then(|d| d.get("AudioComponents"))
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.as_dictionary())
        .and_then(|c| c.get("name"))
        .and_then(|n| n.as_string())
    {
        if let Some((vendor, _)) = name.split_once(':') {
            return vendor.trim().to_string();
        }
    }
    // Fallback: the org segment of the bundle identifier ("com.fabfilter.…").
    // Reversed-country IDs ("uk.co.vendor.…") put a TLD in second position;
    // skip past it to the real org.
    let parts: Vec<&str> = bundle_id.split('.').collect();
    let org = match parts.as_slice() {
        [_, tld, org, ..] if ["co", "com", "net", "org"].contains(tld) => Some(*org),
        [_, org, ..] => Some(*org),
        _ => None,
    };
    if let Some(org) = org {
        let mut chars = org.chars();
        if let Some(first) = chars.next() {
            return first.to_uppercase().collect::<String>() + chars.as_str();
        }
    }
    "?".to_string()
}

/// Some vendors (notably Arturia AUs) store the version as a packed 32-bit
/// integer: 0x00010401 → "1.4.1". Decode when the string is a bare integer
/// too large to be a plain major version.
fn normalize_version(raw: String) -> String {
    if raw.len() >= 5 && raw.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(v) = raw.parse::<u32>() {
            if v >= 0x10000 {
                return format!("{}.{}.{}", v >> 16, (v >> 8) & 0xff, v & 0xff);
            }
        }
    }
    // XILS-lab style hex: "0x010103" → 1.1.3
    if let Some(hex) = raw.strip_prefix("0x") {
        if hex.len() >= 5 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(v) = u32::from_str_radix(hex, 16) {
                return format!("{}.{}.{}", v >> 16, (v >> 8) & 0xff, v & 0xff);
            }
        }
    }
    // Some vendors pollute CFBundleShortVersionString with trailing text
    // (Soundtoys: "5.5.5.19885 Authorization: EchoBoy"). When the string
    // starts with a dotted-numeric version, keep only that leading run.
    if raw
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
        && raw.contains(|c: char| !c.is_ascii_digit() && c != '.')
    {
        let leading: String = raw
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let trimmed = leading.trim_end_matches('.');
        if trimmed.contains('.') && !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    raw
}

/// Some vendors bake the plugin format into the bundle name ("ADAPTIVERB AU",
/// "BC Chorus 4 VST3(Mono)"). Strip the format token so those group into one
/// product; channel qualifiers like "(Mono)" stay, since those coexist.
fn normalize_product_name(name: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;
    static PAREN: OnceLock<Regex> = OnceLock::new();
    static TAIL: OnceLock<Regex> = OnceLock::new();
    let paren = PAREN.get_or_init(|| Regex::new(r" (AU|VST3|VST2|VST)\(").unwrap());
    let tail = TAIL.get_or_init(|| Regex::new(r" (AU|VST3|VST2|VST)$").unwrap());
    let s = paren.replace(name, " (").into_owned();
    tail.replace(&s, "").into_owned()
}

fn read_bundle(path: &Path, format: &'static str, mtime: i64) -> ScannedBundle {
    let name = crate::util::sanitize_line(&normalize_product_name(
        path.file_stem().and_then(|s| s.to_str()).unwrap_or("?"),
    ));
    let path_str = path.display().to_string();
    match Value::from_file(path.join("Contents/Info.plist")) {
        Ok(v) => {
            let d = v.as_dictionary();
            let get = |key: &str| {
                d.and_then(|d| d.get(key))
                    .and_then(|v| v.as_string())
                    .map(str::to_string)
            };
            // Bundle metadata is untrusted input (any installer writes it);
            // sanitize before it can reach the catalog or a terminal.
            let get = |key: &str| get(key).map(|s| crate::util::sanitize_line(&s));
            let bundle_id = get("CFBundleIdentifier");
            let vendor_raw = vendor_from(bundle_id.as_deref().unwrap_or(""), &v);
            ScannedBundle {
                path: path_str,
                format,
                name,
                version: get("CFBundleShortVersionString")
                    .or_else(|| get("CFBundleVersion"))
                    .map(normalize_version),
                bundle_id,
                vendor: vendors::canonical(&crate::util::sanitize_line(&vendor_raw)),
                mtime,
            }
        }
        Err(_) => ScannedBundle {
            path: path_str,
            format,
            name,
            version: None,
            bundle_id: None,
            vendor: "?".to_string(),
            mtime,
        },
    }
}

pub fn run(conn: &mut Connection, full: bool) -> rusqlite::Result<()> {
    let t0 = Instant::now();
    let now = unix_now();

    // Discover bundles on disk.
    let mut found: Vec<(PathBuf, &'static str, i64)> = Vec::new();
    for root in search_roots() {
        let mut bundles = Vec::new();
        find_bundles(&root, 0, &mut bundles);
        found.extend(bundles.into_iter().map(|(p, f)| {
            let mtime = mtime_of(&p);
            (p, f, mtime)
        }));
    }
    let waves = find_waves_plugins();
    let seen_waves: Vec<String> = waves.iter().map(|(p, _)| p.display().to_string()).collect();
    let daw_apps = find_daw_apps();
    let seen_daw: Vec<String> = daw_apps
        .iter()
        .map(|(p, _, _)| p.display().to_string())
        .collect();
    let seen_paths: HashSet<String> = seen_daw
        .iter()
        .cloned()
        .chain(seen_waves.iter().cloned())
        .chain(found.iter().map(|(p, _, _)| p.display().to_string()))
        .collect();

    // Existing active catalog rows, for the mtime cache and removal diff.
    let mut existing: HashMap<String, (i64, i64, Option<String>)> = HashMap::new();
    {
        let mut stmt =
            conn.prepare("SELECT path, id, mtime, version FROM plugins WHERE removed_at IS NULL")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ),
            ))
        })?;
        for row in rows {
            let (path, v) = row?;
            existing.insert(path, v);
        }
    }

    // Partition: unchanged bundles skip the plist read entirely.
    let mut unchanged_ids: Vec<i64> = Vec::new();
    let mut to_read: Vec<(PathBuf, &'static str, i64)> = Vec::new();
    for (path, fmt, mtime) in found {
        let key = path.display().to_string();
        match existing.get(&key) {
            Some((id, cached_mtime, _)) if !full && *cached_mtime == mtime => {
                unchanged_ids.push(*id)
            }
            _ => to_read.push((path, fmt, mtime)),
        }
    }

    let mut scanned: Vec<ScannedBundle> = to_read
        .par_iter()
        .map(|(path, fmt, mtime)| read_bundle(path, fmt, *mtime))
        .collect();
    for (path, vendor, product) in &daw_apps {
        let mtime = mtime_of(path);
        let key = path.display().to_string();
        match existing.get(&key) {
            Some((id, cached_mtime, _)) if !full && *cached_mtime == mtime => {
                unchanged_ids.push(*id)
            }
            _ => scanned.push(read_app(path, vendor, product, mtime)),
        }
    }
    for (path, _) in &waves {
        let mtime = mtime_of(path);
        let key = path.display().to_string();
        match existing.get(&key) {
            Some((id, cached_mtime, _)) if !full && *cached_mtime == mtime => {
                unchanged_ids.push(*id)
            }
            _ => scanned.push(read_waves_plugin(path, mtime)),
        }
    }

    // Reconcile.
    let tx = conn.transaction()?;
    // Vendors merge on normalized key so metadata variants ("UVI"/"Uvi",
    // "D16 Group"/"D16group") land on one row; first-seen display name wins
    // (AU names scan first and are the prettiest).
    let mut vendor_ids: HashMap<String, i64> = HashMap::new();
    {
        let mut stmt = tx.prepare("SELECT name, id FROM vendors")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (name, id) = row?;
            vendor_ids.entry(vendors::normkey(&name)).or_insert(id);
        }
    }
    let (mut added, mut changed) = (0u32, 0u32);
    for sb in &scanned {
        // Bundles with junk vendor metadata ("Vst3", unreadable plists) adopt
        // the vendor of the same-named product that already exists under a
        // real vendor, or an explicit product-name override.
        let mut vendor_name = sb.vendor.clone();
        if vendors::is_junk_vendor(&vendor_name) {
            if let Some(v) = vendors::vendor_for_product(&sb.name) {
                vendor_name = v.to_string();
            } else if let Ok(v) = tx.query_row(
                "SELECT v.name FROM bundles p JOIN vendors v ON v.id = p.vendor_id
                 WHERE p.name = ?1 AND v.name NOT IN ('Vst3', '?') LIMIT 1",
                [&sb.name],
                |r| r.get::<_, String>(0),
            ) {
                vendor_name = v;
            }
        }
        let vkey = vendors::normkey(&vendor_name);
        let vendor_id: i64 = match vendor_ids.get(&vkey) {
            Some(id) => *id,
            None => {
                tx.execute(
                    "INSERT INTO vendors(name, manager_app) VALUES(?1, ?2)",
                    params![vendor_name, vendors::manager_app(&vendor_name)],
                )?;
                let id = tx.last_insert_rowid();
                vendor_ids.insert(vkey, id);
                id
            }
        };
        tx.execute(
            "INSERT OR IGNORE INTO bundles(vendor_id, name) VALUES(?1, ?2)",
            params![vendor_id, sb.name],
        )?;
        let bundle_id: i64 = tx.query_row(
            "SELECT id FROM bundles WHERE vendor_id = ?1 AND name = ?2",
            params![vendor_id, sb.name],
            |r| r.get(0),
        )?;
        match existing.get(&sb.path) {
            Some((id, _, old_version)) => {
                if *old_version != sb.version {
                    changed += 1;
                }
                tx.execute(
                    "UPDATE plugins SET bundle_id=?1, version=?2, mac_bundle_id=?3, mtime=?4,
                     last_seen=?5, removed_at=NULL WHERE id=?6",
                    params![bundle_id, sb.version, sb.bundle_id, sb.mtime, now, id],
                )?;
            }
            None => {
                added += 1;
                tx.execute(
                    "INSERT INTO plugins(bundle_id, path, format, version, mac_bundle_id, mtime,
                     first_seen, last_seen)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                     ON CONFLICT(path) DO UPDATE SET bundle_id=excluded.bundle_id,
                       version=excluded.version, bundle_id=excluded.bundle_id,
                       mtime=excluded.mtime, last_seen=excluded.last_seen, removed_at=NULL",
                    params![
                        bundle_id,
                        sb.path,
                        sb.format,
                        sb.version,
                        sb.bundle_id,
                        sb.mtime,
                        now
                    ],
                )?;
            }
        }
    }
    {
        let mut touch = tx.prepare("UPDATE plugins SET last_seen=?1 WHERE id=?2")?;
        for id in &unchanged_ids {
            touch.execute(params![now, id])?;
        }
    }
    let mut removed = 0u32;
    {
        let mut mark = tx.prepare("UPDATE plugins SET removed_at=?1 WHERE id=?2")?;
        for (path, (id, _, _)) in &existing {
            if !seen_paths.contains(path) {
                mark.execute(params![now, id])?;
                removed += 1;
            }
        }
    }
    let total = scanned.len() + unchanged_ids.len();
    let duration_ms = t0.elapsed().as_millis() as i64;
    tx.execute(
        "INSERT INTO scans(started_at, duration_ms, found, added, removed, changed)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![now, duration_ms, total as i64, added, removed, changed],
    )?;
    tx.commit()?;

    println!(
        "Scanned {} plugins in {} ms ({} read, {} cached)",
        total,
        duration_ms,
        scanned.len(),
        unchanged_ids.len()
    );
    println!("  added: {added}   removed: {removed}   version-changed: {changed}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn packed_decimal_examples() {
        // Arturia-style packed 32-bit: 0x00010401 = 66561 -> "1.4.1"
        assert_eq!(normalize_version("66561".into()), "1.4.1");
        assert_eq!(normalize_version("262146".into()), "4.0.2");
        // small ints and dotted strings pass through
        assert_eq!(normalize_version("2".into()), "2");
        assert_eq!(normalize_version("1.4.1.6566".into()), "1.4.1.6566");
    }

    #[test]
    fn polluted_short_version_cleaned() {
        // Soundtoys stuffs junk into CFBundleShortVersionString
        assert_eq!(
            normalize_version("5.5.5.19885 Authorization: EchoBoy".into()),
            "5.5.5.19885"
        );
        // a normal version with no junk is untouched
        assert_eq!(normalize_version("1.4.8".into()), "1.4.8");
        // a single number with trailing text is NOT split (no dotted version)
        assert_eq!(normalize_version("2 beta".into()), "2 beta");
    }

    #[test]
    fn packed_hex_examples() {
        // XILS-style: 0x010103 -> "1.1.3"
        assert_eq!(normalize_version("0x010103".into()), "1.1.3");
        assert_eq!(normalize_version("0x020000".into()), "2.0.0");
    }

    #[test]
    fn product_name_grouping() {
        // format suffix stripped so variants group into one bundle
        assert_eq!(normalize_product_name("ADAPTIVERB AU"), "ADAPTIVERB");
        assert_eq!(
            normalize_product_name("BC Chorus 4 VST3(Mono)"),
            "BC Chorus 4 (Mono)"
        );
        // channel qualifier and plain names untouched
        assert_eq!(normalize_product_name("Pro-Q 4"), "Pro-Q 4");
    }

    proptest! {
        // normalize_product_name is idempotent.
        #[test]
        fn product_name_idempotent(s in "[A-Za-z0-9 ()-]{1,40}") {
            let n = normalize_product_name(&s);
            prop_assert_eq!(normalize_product_name(&n), n.clone());
        }

        // A decoded packed version always has three dot-separated numeric parts.
        #[test]
        fn packed_decodes_to_triple(v in 0x10000u32..=0xffffffu32) {
            let decoded = normalize_version(v.to_string());
            let parts: Vec<&str> = decoded.split('.').collect();
            prop_assert_eq!(parts.len(), 3);
            prop_assert!(parts.iter().all(|p| p.parse::<u32>().is_ok()));
        }

        // Already-dotted versions are never mangled.
        #[test]
        fn dotted_passthrough(a in 0u32..99, b in 0u32..99, c in 0u32..99) {
            let v = format!("{a}.{b}.{c}");
            prop_assert_eq!(normalize_version(v.clone()), v);
        }
    }
}
