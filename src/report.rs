use rusqlite::{params, Connection};
use serde::Serialize;
use std::cmp::Ordering;

/// Lenient version comparison: split on non-digits, compare numerically,
/// missing segments count as zero ("1.4.1.6566" > "1.4.1", "4.13" > "4.2").
/// Compare only as many segments as the shorter version publishes: a vendor
/// page saying "3.5" is not older than an installed "3.5.1" — vendors often
/// publish major.minor only. Use for staleness decisions; use cmp_versions
/// for picking the max of fully-specified versions.
pub fn cmp_versions_prefix(a: &str, b: &str) -> Ordering {
    let nums = |s: &str| -> Vec<u64> {
        s.split(|c: char| !c.is_ascii_digit())
            .filter(|t| !t.is_empty())
            .map(|t| t.parse().unwrap_or(0))
            .collect()
    };
    let (va, vb) = (nums(a), nums(b));
    let n = va.len().min(vb.len()).max(1);
    for i in 0..n {
        let x = va.get(i).copied().unwrap_or(0);
        let y = vb.get(i).copied().unwrap_or(0);
        if x != y {
            return x.cmp(&y);
        }
    }
    Ordering::Equal
}

/// A "latest" whose major version leaps implausibly far past the installed
/// major is almost certainly a wrong capture (a theme asset ?ver=, an
/// unrelated number on a redesigned page), not a real release. Guards against
/// forward false-positives the way the never-report-backwards rule guards
/// backward ones. Bound is generous: real major bumps are +1..+3; asset noise
/// is +20 or more against a 1.x-3.x install.
pub fn implausible_jump(installed: &str, latest: &str) -> bool {
    let major = |s: &str| -> Option<u64> {
        s.split(|c: char| !c.is_ascii_digit())
            .find(|t| !t.is_empty())
            .and_then(|t| t.parse().ok())
    };
    match (major(installed), major(latest)) {
        (Some(i), Some(l)) => l > i + 20,
        _ => false,
    }
}

pub fn cmp_versions(a: &str, b: &str) -> Ordering {
    let nums = |s: &str| -> Vec<u64> {
        s.split(|c: char| !c.is_ascii_digit())
            .filter(|t| !t.is_empty())
            .map(|t| t.parse().unwrap_or(0))
            .collect()
    };
    let (va, vb) = (nums(a), nums(b));
    for i in 0..va.len().max(vb.len()) {
        let x = va.get(i).copied().unwrap_or(0);
        let y = vb.get(i).copied().unwrap_or(0);
        if x != y {
            return x.cmp(&y);
        }
    }
    Ordering::Equal
}

#[derive(Serialize)]
struct BundleRow {
    vendor: String,
    bundle: String,
    formats: String,
    versions: String,
}

fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }
    let line = |cells: &[String]| {
        let mut s = String::new();
        for (i, cell) in cells.iter().enumerate() {
            s.push_str(&format!("{:<w$}  ", cell, w = widths[i]));
        }
        println!("{}", s.trim_end());
    };
    line(&headers.iter().map(|h| h.to_string()).collect::<Vec<_>>());
    line(&widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>());
    for row in rows {
        line(row);
    }
}

pub fn list(
    conn: &Connection,
    vendor: Option<&str>,
    format: Option<&str>,
    search: Option<&str>,
    json: bool,
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT v.name, p.name,
                group_concat(DISTINCT b.format),
                group_concat(DISTINCT COALESCE(b.version, '?'))
         FROM bundles p
         JOIN vendors v ON v.id = p.vendor_id
         JOIN plugins b ON b.bundle_id = p.id AND b.removed_at IS NULL
         WHERE (?1 IS NULL OR v.name LIKE '%' || ?1 || '%')
           AND (?2 IS NULL OR p.name LIKE '%' || ?2 || '%')
           AND (?3 IS NULL OR EXISTS(
                 SELECT 1 FROM plugins b2
                 WHERE b2.bundle_id = p.id AND b2.removed_at IS NULL
                   AND b2.format = ?3 COLLATE NOCASE))
         GROUP BY p.id
         ORDER BY v.name COLLATE NOCASE, p.name COLLATE NOCASE",
    )?;
    let rows: Vec<BundleRow> = stmt
        .query_map(params![vendor, search, format], |r| {
            Ok(BundleRow {
                vendor: r.get(0)?,
                bundle: r.get(1)?,
                formats: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                versions: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            })
        })?
        .collect::<Result<_, _>>()?;

    if json {
        println!("{}", serde_json::to_string_pretty(&rows).unwrap());
    } else {
        let table: Vec<Vec<String>> = rows
            .iter()
            .map(|r| {
                vec![
                    r.vendor.clone(),
                    r.bundle.clone(),
                    r.formats.clone(),
                    r.versions.clone(),
                ]
            })
            .collect();
        print_table(&["VENDOR", "BUNDLE", "FORMATS", "VERSION"], &table);
        println!("\n{} bundles", rows.len());
    }
    Ok(())
}

#[derive(Serialize)]
struct PluginDetail {
    format: String,
    version: Option<String>,
    mac_bundle_id: Option<String>,
    path: String,
}

#[derive(Serialize)]
struct BundleDetail {
    vendor: String,
    bundle: String,
    manager_app: Option<String>,
    plugins: Vec<PluginDetail>,
}

pub fn info(conn: &Connection, name: &str, json: bool) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT p.id, v.name, p.name, v.manager_app
         FROM bundles p JOIN vendors v ON v.id = p.vendor_id
         WHERE p.name LIKE '%' || ?1 || '%'
         ORDER BY v.name COLLATE NOCASE, p.name COLLATE NOCASE",
    )?;
    let bundles: Vec<(i64, String, String, Option<String>)> = stmt
        .query_map([name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;

    let mut details = Vec::new();
    for (pid, vendor, bundle, manager_app) in bundles {
        let mut bstmt = conn.prepare(
            "SELECT format, version, mac_bundle_id, path FROM plugins
             WHERE bundle_id = ?1 AND removed_at IS NULL ORDER BY format",
        )?;
        let plugins: Vec<PluginDetail> = bstmt
            .query_map([pid], |r| {
                Ok(PluginDetail {
                    format: r.get(0)?,
                    version: r.get(1)?,
                    mac_bundle_id: r.get(2)?,
                    path: r.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        details.push(BundleDetail {
            vendor,
            bundle,
            manager_app,
            plugins,
        });
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&details).unwrap());
    } else if details.is_empty() {
        println!("No bundle matching \"{name}\"");
    } else {
        for d in &details {
            println!("\n{} — {}", d.vendor, d.bundle);
            if let Some(m) = &d.manager_app {
                println!("  managed by: {m}");
            }
            for b in &d.plugins {
                println!(
                    "  {:<5} {:<12} {}",
                    b.format,
                    b.version.as_deref().unwrap_or("?"),
                    b.path
                );
            }
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct DoctorReport {
    version_mismatches: Vec<String>,
    unknown_versions: Vec<String>,
    duplicates: Vec<String>,
    recently_removed: Vec<String>,
}

pub fn doctor(conn: &Connection, json: bool) -> rusqlite::Result<()> {
    let collect = |sql: &str| -> rusqlite::Result<Vec<String>> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect()
    };

    let report = DoctorReport {
        version_mismatches: collect(
            "SELECT v.name || ' ' || p.name || ': ' ||
                    group_concat(b.format || '=' || COALESCE(b.version, '?'))
             FROM bundles p
             JOIN vendors v ON v.id = p.vendor_id
             JOIN plugins b ON b.bundle_id = p.id AND b.removed_at IS NULL
             GROUP BY p.id
             HAVING count(DISTINCT COALESCE(b.version, '?')) > 1
             ORDER BY v.name COLLATE NOCASE",
        )?,
        unknown_versions: collect(
            "SELECT v.name || ' ' || p.name || ' (' || b.format || ')'
             FROM plugins b
             JOIN bundles p ON p.id = b.bundle_id
             JOIN vendors v ON v.id = p.vendor_id
             WHERE b.removed_at IS NULL AND b.version IS NULL
             ORDER BY v.name COLLATE NOCASE",
        )?,
        duplicates: collect(
            "SELECT v.name || ' ' || p.name || ' (' || b.format || '): ' ||
                    group_concat(b.path, ' | ')
             FROM plugins b
             JOIN bundles p ON p.id = b.bundle_id
             JOIN vendors v ON v.id = p.vendor_id
             WHERE b.removed_at IS NULL AND p.name != 'Waves Shell'
             GROUP BY p.id, b.format
             HAVING count(*) > 1
             ORDER BY v.name COLLATE NOCASE",
        )?,
        recently_removed: collect(
            "SELECT v.name || ' ' || p.name || ' (' || b.format || ')'
             FROM plugins b
             JOIN bundles p ON p.id = b.bundle_id
             JOIN vendors v ON v.id = p.vendor_id
             WHERE b.removed_at = (SELECT max(removed_at) FROM plugins WHERE removed_at IS NOT NULL)
             ORDER BY v.name COLLATE NOCASE",
        )?,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    } else {
        let section = |title: &str, items: &[String]| {
            println!("\n{title} ({})", items.len());
            for i in items.iter().take(50) {
                println!("  {i}");
            }
            if items.len() > 50 {
                println!("  … and {} more (use --json for all)", items.len() - 50);
            }
        };
        section(
            "VERSION MISMATCH ACROSS FORMATS",
            &report.version_mismatches,
        );
        section("UNKNOWN VERSION", &report.unknown_versions);
        section("DUPLICATE INSTALLS", &report.duplicates);
        section("REMOVED IN LAST RECONCILIATION", &report.recently_removed);
    }
    Ok(())
}

#[derive(Serialize)]
struct VendorRow {
    vendor: String,
    bundles: i64,
    plugins: i64,
    manager_app: Option<String>,
}

pub fn vendors(conn: &Connection, json: bool) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT v.name, count(DISTINCT p.id), count(b.id), v.manager_app
         FROM vendors v
         JOIN bundles p ON p.vendor_id = v.id
         JOIN plugins b ON b.bundle_id = p.id AND b.removed_at IS NULL
         GROUP BY v.id
         ORDER BY count(b.id) DESC, v.name COLLATE NOCASE",
    )?;
    let rows: Vec<VendorRow> = stmt
        .query_map([], |r| {
            Ok(VendorRow {
                vendor: r.get(0)?,
                bundles: r.get(1)?,
                plugins: r.get(2)?,
                manager_app: r.get(3)?,
            })
        })?
        .collect::<Result<_, _>>()?;

    if json {
        println!("{}", serde_json::to_string_pretty(&rows).unwrap());
    } else {
        let table: Vec<Vec<String>> = rows
            .iter()
            .map(|r| {
                vec![
                    r.vendor.clone(),
                    r.bundles.to_string(),
                    r.plugins.to_string(),
                    r.manager_app.clone().unwrap_or_default(),
                ]
            })
            .collect();
        print_table(&["VENDOR", "BUNDLES", "PLUGINS", "MANAGER"], &table);
        println!("\n{} vendors", rows.len());
    }
    Ok(())
}

#[derive(Serialize)]
struct StaleRow {
    vendor: String,
    bundle: String,
    installed: String,
    latest: String,
    url: Option<String>,
    via: Option<String>,
}

#[derive(Serialize)]
struct ManagedRow {
    vendor: String,
    bundles: i64,
    manager_app: String,
}

#[derive(Serialize)]
struct OutdatedReport {
    stale: Vec<StaleRow>,
    unconfirmed: Vec<StaleRow>,
    pinned_stale: Vec<String>,
    resolver_stale: Vec<String>,
    resolver_suspect: Vec<String>,
    paid_upgrade: Vec<String>,
    manual_check: Vec<(String, i64)>,
    managed: Vec<ManagedRow>,
    unresolved_vendors: i64,
    unresolved_products: i64,
    up_to_date: i64,
}

pub fn outdated(conn: &Connection, json: bool, explain: bool) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT v.name, p.name,
                group_concat(DISTINCT COALESCE(b.version, '?')),
                c.latest_version, c.url, c.source, c.paid_from,
                COALESCE(um.ignored, 0), COALESCE(um.pinned, 0),
                v.manager_app
         FROM bundles p
         JOIN vendors v ON v.id = p.vendor_id
         JOIN plugins b ON b.bundle_id = p.id AND b.removed_at IS NULL
         LEFT JOIN checks c ON c.bundle_id = p.id
         LEFT JOIN user_meta um ON um.bundle_id = p.id
         GROUP BY p.id
         ORDER BY v.name COLLATE NOCASE, p.name COLLATE NOCASE",
    )?;
    struct Row {
        vendor: String,
        bundle: String,
        versions: String,
        latest: Option<String>,
        url: Option<String>,
        source: Option<String>,
        paid_from: Option<String>,
        ignored: bool,
        pinned: bool,
        manager: Option<String>,
    }
    let rows: Vec<Row> = stmt
        .query_map([], |r| {
            Ok(Row {
                vendor: r.get(0)?,
                bundle: r.get(1)?,
                versions: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                latest: r.get(3)?,
                url: r.get(4)?,
                source: r.get(5)?,
                paid_from: r.get(6)?,
                ignored: r.get::<_, i64>(7)? != 0,
                pinned: r.get::<_, i64>(8)? != 0,
                manager: r.get(9)?,
            })
        })?
        .collect::<Result<_, _>>()?;

    let mut report = OutdatedReport {
        stale: Vec::new(),
        unconfirmed: Vec::new(),
        pinned_stale: Vec::new(),
        resolver_stale: Vec::new(),
        resolver_suspect: Vec::new(),
        paid_upgrade: Vec::new(),
        manual_check: Vec::new(),
        managed: Vec::new(),
        unresolved_vendors: 0,
        unresolved_products: 0,
        up_to_date: 0,
    };
    let mut managed: std::collections::BTreeMap<String, (i64, String)> = Default::default();
    let mut unresolved: std::collections::BTreeMap<String, i64> = Default::default();
    // Vendors with a resolver file but no automatable bundles: they publish
    // no version info, but we know exactly how a human updates them.
    let resolver_files = crate::resolver::load_all();
    let manual_vendors: std::collections::HashSet<String> = resolver_files
        .iter()
        .filter(|rf| rf.manual.is_some() || rf.skip.is_some())
        .map(|rf| rf.vendor.clone())
        .collect();
    let download_via: std::collections::HashMap<String, String> = resolver_files
        .iter()
        .filter_map(|rf| rf.download_via.clone().map(|v| (rf.vendor.clone(), v)))
        .collect();
    let mut manual_counts: std::collections::BTreeMap<String, i64> = Default::default();

    for row in &rows {
        if row.ignored {
            continue;
        }
        let installed = row
            .versions
            .split(',')
            .max_by(|a, b| cmp_versions(a, b))
            .unwrap_or("?")
            .to_string();
        match &row.latest {
            Some(latest)
                if cmp_versions_prefix(latest, &installed) == Ordering::Greater
                    && implausible_jump(&installed, latest) =>
            {
                report.resolver_suspect.push(format!(
                    "{} {} (page says {latest}, installed {installed} — implausible jump, likely wrong capture)",
                    row.vendor, row.bundle
                ));
            }
            Some(latest)
                if cmp_versions_prefix(latest, &installed) == Ordering::Greater
                    && row.paid_from.as_deref().is_some_and(|pf| {
                        cmp_versions(&installed, pf) == Ordering::Less
                            && cmp_versions(latest, pf) != Ordering::Less
                    }) =>
            {
                report.paid_upgrade.push(format!(
                    "{} {} ({installed} → {latest}, paid major upgrade)",
                    row.vendor, row.bundle
                ));
            }
            Some(latest) if cmp_versions_prefix(latest, &installed) == Ordering::Greater => {
                if row.pinned {
                    report
                        .pinned_stale
                        .push(format!("{} {}", row.vendor, row.bundle));
                } else {
                    let via = row
                        .source
                        .as_deref()
                        .and_then(|s| s.split_once(" via "))
                        .map(|(_, v)| v.to_string());
                    let sr = StaleRow {
                        vendor: row.vendor.clone(),
                        bundle: row.bundle.clone(),
                        installed,
                        latest: latest.clone(),
                        url: row.url.clone(),
                        via: via.clone(),
                    };
                    // Third-party sources (KVR) are developer-submitted and
                    // demonstrably run ahead of actual macOS releases; never
                    // assert a firm update from them, only a possibility.
                    if via.is_some() {
                        report.unconfirmed.push(sr);
                    } else {
                        report.stale.push(sr);
                    }
                }
            }
            // Vendor page trails the installed version: the resolver is
            // stale or the page lies — never shown as an update.
            Some(latest) if cmp_versions_prefix(latest, &installed) == Ordering::Less => {
                report.resolver_stale.push(format!(
                    "{} {} (page says {latest}, installed {installed})",
                    row.vendor, row.bundle
                ));
            }
            Some(_) => report.up_to_date += 1,
            None => match &row.manager {
                Some(m) => {
                    let e = managed.entry(row.vendor.clone()).or_insert((0, m.clone()));
                    e.0 += 1;
                }
                None if manual_vendors.contains(&row.vendor) => {
                    *manual_counts.entry(row.vendor.clone()).or_insert(0) += 1;
                }
                None => {
                    *unresolved.entry(row.vendor.clone()).or_insert(0) += 1;
                }
            },
        }
    }
    for (vendor, (count, manager_app)) in managed {
        report.managed.push(ManagedRow {
            vendor,
            bundles: count,
            manager_app,
        });
    }
    report.managed.sort_by_key(|m| std::cmp::Reverse(m.bundles));
    report.manual_check = manual_counts.into_iter().collect();
    report.manual_check.sort_by_key(|m| std::cmp::Reverse(m.1));
    report.unresolved_vendors = unresolved.len() as i64;
    report.unresolved_products = unresolved.values().sum();

    if json {
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
        return Ok(());
    }

    if report.stale.is_empty() {
        println!("STALE: none known");
    } else {
        println!("STALE ({} bundles)", report.stale.len());
        // Collapse lockstep vendors: when many bundles share one vendor and
        // the same installed→latest transition (Melda's 43 plugins all
        // 17.08→17.09, Soundtoys' 29 at one version), show one line instead
        // of flooding the list. Vendors with their own installer app show it.
        use std::collections::BTreeMap;
        let mut groups: BTreeMap<(String, String, String), Vec<&StaleRow>> = BTreeMap::new();
        for s in &report.stale {
            groups
                .entry((s.vendor.clone(), s.installed.clone(), s.latest.clone()))
                .or_default()
                .push(s);
        }
        for ((vendor, installed, latest), rows) in &groups {
            if rows.len() >= 4 {
                let dest = download_via
                    .get(vendor.as_str())
                    .map(|app| format!("update via {app}"))
                    .unwrap_or_else(|| rows[0].url.clone().unwrap_or_default());
                let via = rows[0]
                    .via
                    .as_deref()
                    .map(|v| format!(" [via {v}]"))
                    .unwrap_or_default();
                println!(
                    "  {:<18} {} plugins  {installed} → {latest}{via}    {dest}",
                    vendor,
                    rows.len()
                );
            } else {
                for s in rows {
                    println!(
                        "  {:<18} {:<28} {} → {}{}    {}",
                        s.vendor,
                        s.bundle,
                        s.installed,
                        s.latest,
                        s.via
                            .as_deref()
                            .map(|v| format!(" [via {v}]"))
                            .unwrap_or_default(),
                        s.url.as_deref().unwrap_or("")
                    );
                }
            }
        }
    }
    if explain {
        let manuals: std::collections::BTreeMap<String, crate::resolver::Manual> =
            crate::resolver::load_all()
                .into_iter()
                .filter_map(|rf| rf.manual.map(|m| (rf.vendor, m)))
                .collect();
        let mut vendors_shown: Vec<&String> = report
            .stale
            .iter()
            .map(|s| &s.vendor)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let manual_vendor_names: Vec<&String> =
            report.manual_check.iter().map(|(v, _)| v).collect();
        vendors_shown.extend(manual_vendor_names);
        if report.stale.is_empty() && report.manual_check.is_empty() {
            // Nothing to anchor on: show every vendor that has a walkthrough.
            vendors_shown = manuals.keys().collect();
        }
        let mut printed_header = false;
        for vendor in vendors_shown {
            if let Some(m) = manuals.get(vendor.as_str()) {
                if !printed_header {
                    println!("\nHOW TO UPDATE MANUALLY");
                    printed_header = true;
                }
                println!("  {vendor}:");
                if let Some(login) = &m.login {
                    println!("    login: {login}");
                }
                if let Some(steps) = &m.steps {
                    println!("    {steps}");
                }
            }
        }
    }
    if !report.unconfirmed.is_empty() {
        println!(
            "\nPOSSIBLY OUTDATED ({} — third-party source, unverified; trust the vendor if it disagrees)",
            report.unconfirmed.len()
        );
        use std::collections::BTreeMap as BMap2;
        let mut ug: BMap2<(String, String, String), Vec<&StaleRow>> = BMap2::new();
        for s in &report.unconfirmed {
            ug.entry((s.vendor.clone(), s.installed.clone(), s.latest.clone()))
                .or_default()
                .push(s);
        }
        for ((vendor, installed, latest), rows) in &ug {
            let via = rows[0]
                .via
                .as_deref()
                .map(|v| format!(" [via {v}]"))
                .unwrap_or_default();
            if rows.len() >= 4 {
                println!(
                    "  {:<18} {} plugins  {installed} → {latest}{via}",
                    vendor,
                    rows.len()
                );
            } else {
                for s in rows {
                    println!(
                        "  {:<18} {:<28} {installed} → {latest}{via}",
                        s.vendor, s.bundle
                    );
                }
            }
        }
    }
    if !report.paid_upgrade.is_empty() {
        println!(
            "\nPAID UPGRADE AVAILABLE ({}) — a newer paid major version exists (not a free update)",
            report.paid_upgrade.len()
        );
        for p in report.paid_upgrade.iter().take(30) {
            println!("  {p}");
        }
    }
    if !report.resolver_suspect.is_empty() {
        println!(
            "\nRESOLVER SUSPECT ({}) — latest leaps implausibly past installed; verify the resolver regex",
            report.resolver_suspect.len()
        );
        for r in report.resolver_suspect.iter().take(20) {
            println!("  {r}");
        }
    }
    if !report.resolver_stale.is_empty() {
        println!(
            "\nRESOLVER STALE ({}) — vendor page trails installed; fix or reverify these resolvers",
            report.resolver_stale.len()
        );
        for r in report.resolver_stale.iter().take(20) {
            println!("  {r}");
        }
    }
    if !report.pinned_stale.is_empty() {
        println!(
            "\nPINNED (stale but held): {}",
            report.pinned_stale.join(", ")
        );
    }
    if !report.manual_check.is_empty() {
        println!(
            "\nMANUAL CHECK ({} vendors, {} bundles — no public version info; walkthroughs via --explain)",
            report.manual_check.len(),
            report.manual_check.iter().map(|(_, n)| n).sum::<i64>()
        );
        for (vendor, n) in report.manual_check.iter().take(12) {
            println!("  {vendor:<28} {n:>4} bundles");
        }
        if report.manual_check.len() > 12 {
            println!(
                "  … and {} more (--json for all)",
                report.manual_check.len() - 12
            );
        }
    }
    if !report.managed.is_empty() {
        println!("\nMANAGED ELSEWHERE (launch the manager to update)");
        for m in report.managed.iter().take(15) {
            println!(
                "  {:<24} {:>4} bundles   → {}",
                m.vendor, m.bundles, m.manager_app
            );
        }
    }
    println!(
        "\nUp to date: {}   Unresolved: {} bundles across {} vendors (no resolver yet)",
        report.up_to_date, report.unresolved_products, report.unresolved_vendors
    );
    Ok(())
}

pub fn set_flag(conn: &Connection, name: &str, flag: &str, value: bool) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT p.id, v.name || ' — ' || p.name
         FROM bundles p JOIN vendors v ON v.id = p.vendor_id
         WHERE p.name LIKE '%' || ?1 || '%'",
    )?;
    let matches: Vec<(i64, String)> = stmt
        .query_map([name], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    match matches.len() {
        0 => println!("No bundle matching \"{name}\""),
        1 => {
            let (id, label) = &matches[0];
            // Static SQL per flag: never interpolate identifiers, even
            // internal ones.
            let sql = match flag {
                "pinned" => {
                    "INSERT INTO user_meta(bundle_id, pinned) VALUES(?1, ?2)
                     ON CONFLICT(bundle_id) DO UPDATE SET pinned = excluded.pinned"
                }
                "ignored" => {
                    "INSERT INTO user_meta(bundle_id, ignored) VALUES(?1, ?2)
                     ON CONFLICT(bundle_id) DO UPDATE SET ignored = excluded.ignored"
                }
                _ => unreachable!("unknown user_meta flag"),
            };
            conn.execute(sql, params![id, value as i64])?;
            println!(
                "{} {}: {}",
                if value { "set" } else { "cleared" },
                flag,
                label
            );
        }
        _ => {
            println!(
                "Ambiguous — matches {} bundles, be more specific:",
                matches.len()
            );
            for (_, label) in matches.iter().take(20) {
                println!("  {label}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::cmp::Ordering;

    #[test]
    fn prefix_examples() {
        assert_eq!(cmp_versions_prefix("3.5", "3.5.1"), Ordering::Equal);
        assert_eq!(cmp_versions_prefix("3.5.1", "3.5"), Ordering::Equal);
        assert_eq!(cmp_versions_prefix("4.13", "4.2"), Ordering::Greater);
        assert_eq!(cmp_versions_prefix("1.4.1.6566", "1.4.1"), Ordering::Equal);
    }

    #[test]
    fn full_examples() {
        assert_eq!(cmp_versions("1.0", "1.0.0"), Ordering::Equal);
        assert_eq!(cmp_versions("4.13", "4.2"), Ordering::Greater);
        assert_eq!(cmp_versions("2.0.30", "2.0.9"), Ordering::Greater);
    }

    #[test]
    fn implausible_examples() {
        // theme asset ?ver=26.6 against Diva 1.4.8
        assert!(implausible_jump("1.4.8", "26.6"));
        // real major bumps are fine
        assert!(!implausible_jump("12.4.5", "13.0"));
        assert!(!implausible_jump("3.13.2", "3.14.1"));
        assert!(!implausible_jump("26.1.5", "27.0"));
    }

    proptest! {
        // cmp_versions is a total order: reflexive and antisymmetric.
        #[test]
        fn cmp_reflexive(a in "[0-9]{1,4}(\\.[0-9]{1,4}){0,3}") {
            prop_assert_eq!(cmp_versions(&a, &a), Ordering::Equal);
            prop_assert_eq!(cmp_versions_prefix(&a, &a), Ordering::Equal);
        }

        #[test]
        fn cmp_antisymmetric(
            a in "[0-9]{1,4}(\\.[0-9]{1,4}){0,3}",
            b in "[0-9]{1,4}(\\.[0-9]{1,4}){0,3}",
        ) {
            let ab = cmp_versions(&a, &b);
            let ba = cmp_versions(&b, &a);
            prop_assert_eq!(ab, ba.reverse());
        }

        // Appending a positive component never makes a version smaller under
        // the full comparator (missing components read as zero).
        #[test]
        fn appending_component_is_ge(
            a in "[0-9]{1,4}(\\.[0-9]{1,4}){0,2}",
            extra in 1u32..9999,
        ) {
            let longer = format!("{a}.{extra}");
            prop_assert_ne!(cmp_versions(&longer, &a), Ordering::Less);
        }

        // A backwards move and an implausible jump are mutually exclusive
        // classifications: never both true for the same pair.
        #[test]
        fn suspect_only_forward(
            i in 0u64..40, l in 0u64..200,
        ) {
            let (iv, lv) = (i.to_string(), l.to_string());
            if implausible_jump(&iv, &lv) {
                prop_assert_eq!(cmp_versions_prefix(&lv, &iv), Ordering::Greater);
            }
        }
    }
}
