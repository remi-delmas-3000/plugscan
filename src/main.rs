mod check;
mod db;
mod fetch;
mod report;
mod resolver;
mod scan;
mod util;
mod vendors;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "plugscan",
    version,
    about = "Fast audio plugin inventory (AU / VST / VST3 / CLAP)"
)]
struct Cli {
    /// Path to the catalog database (default: ~/.local/share/plugscan/catalog.db)
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    /// Extra resolver directory (highest priority). The working directory is
    /// never searched implicitly.
    #[arg(long, global = true)]
    resolvers: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan plugin folders and reconcile the catalog
    Scan {
        /// Re-read every bundle, ignoring the mtime cache
        #[arg(long)]
        full: bool,
    },
    /// List bundles in the catalog
    List {
        /// Filter by vendor (substring match)
        #[arg(long)]
        vendor: Option<String>,
        /// Filter by format: AU, VST2, VST3, CLAP
        #[arg(long)]
        format: Option<String>,
        /// Filter by product name (substring match)
        #[arg(long)]
        search: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show details for bundles matching a name
    Info {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Report catalog anomalies (version mismatches, duplicates, unknowns)
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Vendor overview with product/bundle counts and manager apps
    Vendors {
        #[arg(long)]
        json: bool,
    },
    /// Fetch latest versions via resolvers into the catalog
    Check {
        /// Re-check even if a recent result exists
        #[arg(long)]
        force: bool,
        /// Treat checks younger than this as fresh (hours)
        #[arg(long, default_value_t = 24)]
        max_age: i64,
        /// Emit machine-readable results (one object per bundle)
        #[arg(long)]
        json: bool,
    },
    /// Show bundles with known newer versions, plus coverage summary
    Outdated {
        #[arg(long)]
        json: bool,
        /// Include per-vendor manual download walkthroughs
        #[arg(long)]
        explain: bool,
    },
    /// Hold a bundle: stale but deliberately not updated (e.g. mid-project)
    Pin {
        name: String,
        /// Remove the pin
        #[arg(long)]
        undo: bool,
    },
    /// Hide a bundle from outdated reports entirely
    Ignore {
        name: String,
        /// Stop ignoring
        #[arg(long)]
        undo: bool,
    },
    /// Download stale bundles' installers into the local archive
    Fetch {
        /// Only bundles matching this name
        name: Option<String>,
        /// Also open non-direct download pages in the browser
        #[arg(long)]
        open: bool,
        /// Save into this directory (e.g. ".") instead of the archive
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Import a manually-downloaded installer into the archive, or clear it
    Archive {
        /// Installer file to import (omit with --clear)
        file: Option<PathBuf>,
        #[arg(long, alias = "product")]
        bundle: Option<String>,
        #[arg(long)]
        version: Option<String>,
        /// Delete archived installers (all, or --product to narrow) and free the space
        #[arg(long)]
        clear: bool,
    },
    /// Resolver tooling for contributors
    Resolver {
        #[command(subcommand)]
        action: ResolverAction,
    },
}

#[derive(Subcommand)]
enum ResolverAction {
    /// Run a vendor's resolver verbosely: strategy, matches, chosen version
    Debug { vendor: String },
    /// Try strategies against a URL and print a draft resolver TOML
    New {
        vendor: String,
        #[arg(long)]
        url: String,
    },
    /// Exercise every resolver live (no catalog needed); exits non-zero on failures
    Test {
        /// Only this vendor (substring)
        vendor: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

fn default_db_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME not set");
    PathBuf::from(home).join(".local/share/plugscan/catalog.db")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    if let Some(dir) = &cli.resolvers {
        std::env::set_var("PLUGSCAN_RESOLVERS", dir);
    }
    let db_path = cli.db.clone().unwrap_or_else(default_db_path);
    if let Some(dir) = db_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut conn = db::open(&db_path)?;

    match cli.cmd {
        Cmd::Scan { full } => scan::run(&mut conn, full)?,
        Cmd::List {
            vendor,
            format,
            search,
            json,
        } => report::list(
            &conn,
            vendor.as_deref(),
            format.as_deref(),
            search.as_deref(),
            json,
        )?,
        Cmd::Info { name, json } => report::info(&conn, &name, json)?,
        Cmd::Doctor { json } => report::doctor(&conn, json)?,
        Cmd::Vendors { json } => report::vendors(&conn, json)?,
        Cmd::Check { force, max_age, json } => check::run(&mut conn, force, max_age, json)?,
        Cmd::Outdated { json, explain } => report::outdated(&conn, json, explain)?,
        Cmd::Pin { name, undo } => report::set_flag(&conn, &name, "pinned", !undo)?,
        Cmd::Ignore { name, undo } => report::set_flag(&conn, &name, "ignored", !undo)?,
        Cmd::Fetch { name, open, out } => fetch::run(&mut conn, name.as_deref(), open, out.as_deref())?,
        Cmd::Archive { file, bundle, version, clear } => match (clear, file, bundle) {
            (true, _, bundle) => fetch::clear(&mut conn, bundle.as_deref())?,
            (false, Some(file), Some(bundle)) => {
                fetch::import(&mut conn, &file, &bundle, version.as_deref())?
            }
            _ => eprintln!("usage: plugscan archive <file> --product <name>  |  plugscan archive --clear [--product <name>]"),
        },
        Cmd::Resolver { action } => match action {
            ResolverAction::Debug { vendor } => check::debug_vendor(&conn, &vendor)?,
            ResolverAction::New { vendor, url } => check::new_vendor(&vendor, &url),
            ResolverAction::Test { vendor, json } => {
                let failures = check::test_all(json, vendor.as_deref());
                if failures > 0 {
                    std::process::exit(1);
                }
            }
        },
    }
    Ok(())
}
