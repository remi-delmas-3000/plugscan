# plugscan

Fast audio plugin inventory and update awareness for macOS. Scans AU, VST2,
VST3, and CLAP plugin files (no AAX) plus DAW apps by reading their metadata
directly — nothing is ever loaded or instantiated.

Terminology: a **plugin** is one file on disk (`EchoBoy.component`); a
**bundle** groups a plugin's format variants (EchoBoy = AU + VST3 + VST2);
a **vendor** is the manufacturer.

```
plugscan scan               # reconcile the catalog (~350 ms cold, ~20 ms warm)
plugscan list [--vendor u-he] [--format CLAP] [--search "Pro-Q"] [--json]
plugscan info "Pro-Q"       # per-bundle detail with all installed formats
plugscan doctor             # version mismatches, duplicates, unknowns, removals
plugscan vendors            # vendor overview + owning manager app
```

The catalog is a plain SQLite file at `~/.local/share/plugscan/catalog.db` —
open it with any SQLite client. Scans are reconciliations: vanished bundles
are marked removed, never deleted, so history survives.

## Update checking

`check` resolves latest versions via declarative per-vendor TOML resolvers
(`resolvers/`, overridable via `--resolvers` or `~/.config/plugscan/resolvers`;
the working directory is never searched). Strategies: `page_regex` (default),
`json` (+ `json_path`), `github_release`, `sparkle` (appcast), `header`
(version in redirect headers). Prerelease candidates are filtered
(`exclude_regex` to override); `${version}` substitutes into download URLs;
`skip = "reason"` documents vendors that can't be automated. A latest version
that trails the installed one is reported as a stale resolver, never as an
update.

Contributor tooling: `plugscan resolver debug <vendor>` (verbose run) and
`plugscan resolver new <vendor> --url <url>` (strategy cascade → draft TOML).

See SECURITY.md for the trust model.

## Roadmap

- `fetch`: staged, checksummed downloads (public URLs first, browser-session
  cookies later) doubling as an installer archive.
- Sparkle feed autodetection from vendors' companion apps.
- License vault (SQLite metadata + Keychain secrets), paid-upgrade flags.
- launchd scheduling + notifications, migration manifest, DAW project-usage
  scanning, Windows port, `plugfeed` static-feed spec for vendors.

## License

- **Code** (`src/`, everything not listed below): [GPL-3.0-or-later](LICENSE).
- **Resolver database** (`resolvers/`): [MIT](resolvers/LICENSE), so the
  version-checking data stays maximally forkable and reusable by other tools
  and future maintainers.

Dependency policy (inbound) remains MIT/Apache-2.0 only, enforced by
`cargo deny` — see `deny.toml`.
