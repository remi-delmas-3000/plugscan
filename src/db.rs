use rusqlite::Connection;
use std::path::Path;

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    migrate_terminology(&conn)?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

/// Terminology migration (2026-08-31): a file on disk is a *plugin*; the
/// logical unit grouping its format variants is a *bundle* (formerly
/// "product"). Renames tables/columns in place so history survives.
fn migrate_terminology(conn: &Connection) -> rusqlite::Result<()> {
    let old: bool = conn.query_row(
        "SELECT count(*) > 0 FROM sqlite_master WHERE type='table' AND name='products'",
        [],
        |r| r.get(0),
    )?;
    if old {
        conn.execute_batch(
            "ALTER TABLE bundles RENAME TO plugins;
             ALTER TABLE plugins RENAME COLUMN bundle_id TO mac_bundle_id;
             ALTER TABLE plugins RENAME COLUMN product_id TO bundle_id;
             ALTER TABLE products RENAME TO bundles;
             ALTER TABLE checks RENAME COLUMN product_id TO bundle_id;
             ALTER TABLE user_meta RENAME COLUMN product_id TO bundle_id;
             ALTER TABLE downloads RENAME COLUMN product_id TO bundle_id;",
        )?;
    }
    Ok(())
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// The catalog is the system of record; scans are reconciliations. Vanished
// plugins are marked removed_at rather than deleted, so history survives.
// Terminology: plugin = one file on disk; bundle = the logical unit
// grouping a plugin's format variants (AU/VST3/...); vendor = manufacturer.
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS vendors(
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    manager_app TEXT
);
CREATE TABLE IF NOT EXISTS bundles(
    id        INTEGER PRIMARY KEY,
    vendor_id INTEGER NOT NULL REFERENCES vendors(id),
    name      TEXT NOT NULL COLLATE NOCASE,
    UNIQUE(vendor_id, name)
);
CREATE TABLE IF NOT EXISTS plugins(
    id            INTEGER PRIMARY KEY,
    bundle_id     INTEGER NOT NULL REFERENCES bundles(id),
    path          TEXT NOT NULL UNIQUE,
    format        TEXT NOT NULL,
    version       TEXT,
    mac_bundle_id TEXT,
    mtime      INTEGER,
    first_seen INTEGER NOT NULL,
    last_seen  INTEGER NOT NULL,
    removed_at INTEGER
);
CREATE TABLE IF NOT EXISTS scans(
    id          INTEGER PRIMARY KEY,
    started_at  INTEGER NOT NULL,
    duration_ms INTEGER,
    found       INTEGER,
    added       INTEGER,
    removed     INTEGER,
    changed     INTEGER
);
CREATE TABLE IF NOT EXISTS checks(
    bundle_id      INTEGER PRIMARY KEY REFERENCES bundles(id),
    latest_version TEXT,
    url            TEXT,
    source         TEXT,
    checked_at     INTEGER
);
CREATE TABLE IF NOT EXISTS user_meta(
    bundle_id  INTEGER PRIMARY KEY REFERENCES bundles(id),
    pinned     INTEGER NOT NULL DEFAULT 0,
    ignored    INTEGER NOT NULL DEFAULT 0,
    tags       TEXT,
    notes      TEXT
);
CREATE TABLE IF NOT EXISTS downloads(
    id         INTEGER PRIMARY KEY,
    bundle_id  INTEGER NOT NULL REFERENCES bundles(id),
    version    TEXT NOT NULL,
    url        TEXT,
    path       TEXT NOT NULL,
    sha256     TEXT,
    bytes      INTEGER,
    fetched_at INTEGER NOT NULL,
    UNIQUE(bundle_id, version)
);
CREATE INDEX IF NOT EXISTS idx_plugins_bundle ON plugins(bundle_id);
CREATE INDEX IF NOT EXISTS idx_plugins_removed ON plugins(removed_at);
"#;
