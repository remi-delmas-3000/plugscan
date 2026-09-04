// macOS install receipts (/var/db/receipts) record the version a .pkg
// installed, which is authoritative when a vendor ships a bundle whose
// Info.plist version was never bumped. Observed with Ignite Amps Libra: the
// 1.3.0 installer's receipt says 1.3.0 while the installed bundle's
// CFBundleShortVersionString still says 1.2.0. This maps each plugin bundle
// installed by a pkg to the receipt's version, so scan can prefer it over a
// stale plist.

use plist::Value;
use std::collections::HashMap;
use std::process::Command;

const RECEIPTS_DIR: &str = "/var/db/receipts";

/// Plugin-bundle path -> version, from every receipt that installed into a
/// plugin folder. Cheap: only receipts whose InstallPrefixPath is under
/// Plug-Ins are inspected (a few dozen of the thousands present), and only
/// those get an lsbom.
pub fn plugin_bundle_versions() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(entries) = std::fs::read_dir(RECEIPTS_DIR) else {
        return map;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("plist") {
            continue;
        }
        let Ok(v) = Value::from_file(&path) else {
            continue;
        };
        let dict = match v.as_dictionary() {
            Some(d) => d,
            None => continue,
        };
        let prefix = dict
            .get("InstallPrefixPath")
            .and_then(|v| v.as_string())
            .unwrap_or("");
        // Only receipts that installed into an audio plugin folder matter.
        if !prefix.contains("Plug-Ins") {
            continue;
        }
        let version = match dict.get("PackageVersion").and_then(|v| v.as_string()) {
            Some(v) => v.to_string(),
            None => continue,
        };
        // The matching .bom lists the installed files; the top-level bundle
        // dirs are what we install-map. lsbom is only run for this small
        // plugin-folder subset.
        let bom = path.with_extension("bom");
        let Ok(out) = Command::new("lsbom").arg("-s").arg(&bom).output() else {
            continue;
        };
        let listing = String::from_utf8_lossy(&out.stdout);
        for line in listing.lines() {
            let rel = line.trim().trim_start_matches("./");
            // The bundle is the first path component ending in a plugin ext.
            let top = rel.split('/').next().unwrap_or("");
            let is_bundle = [".component", ".vst3", ".vst", ".clap", ".aaxplugin"]
                .iter()
                .any(|ext| top.ends_with(ext));
            if !is_bundle {
                continue;
            }
            let abs = format!("/{}/{}", prefix.trim_matches('/'), top);
            // Keep the highest version if several receipts touch one path.
            match map.get(&abs) {
                Some(existing) if crate::report::cmp_versions(&version, existing)
                    != std::cmp::Ordering::Greater => {}
                _ => {
                    map.insert(abs, version.clone());
                }
            }
        }
    }
    map
}

fn major(v: &str) -> Option<u64> {
    v.split(|c: char| !c.is_ascii_digit())
        .find(|t| !t.is_empty())
        .and_then(|t| t.parse().ok())
}

/// True when the receipt is a strictly newer version than the bundle's plist
/// AND shares the same major version. The same-major guard is the safety
/// rail: a pkg's PackageVersion is not guaranteed to follow the plugin's
/// version scheme (some are "0", build numbers, or year-based), so only trust
/// it to bump a stale plist forward within the same major line (Libra plist
/// 1.2.0 vs receipt 1.3.0). Cross-major disagreements are left alone.
pub fn receipt_supersedes(plist: Option<&str>, receipt: &str) -> bool {
    match plist {
        Some(p) => {
            major(p).is_some()
                && major(p) == major(receipt)
                && crate::report::cmp_versions(receipt, p) == std::cmp::Ordering::Greater
        }
        // No plist version to trust: never invent one from a receipt (could
        // be "0"); leave it unknown rather than wrong.
        None => false,
    }
}
