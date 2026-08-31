# Security model

plugscan tells people what software is outdated and where to get the new
version. That makes it a supply-chain component: the worst realistic outcome
is a user being steered to install something malicious. Everything here is
organized around preventing that.

## Trust boundaries

| Input | Trust | Handling |
|---|---|---|
| Installed bundle metadata (Info.plist, file names) | Untrusted (any installer writes it) | Read-only parse; control characters stripped at ingestion; plugins are never loaded or executed |
| Resolver TOMLs | Semi-trusted (shipped set is reviewed; users can add their own) | Validated at load: https-only URLs, control characters stripped, non-conforming products dropped with a warning |
| Fetched web pages | Untrusted | Only a regex capture escapes them; the capture is sanitized. Page content is never executed, rendered, or fed to anything that follows instructions |
| The catalog DB | Local file | All SQL parameterized; no identifiers interpolated |

## Controls in the code

- **No CWD resolver loading.** Resolver dirs are: `--resolvers` flag /
  `$PLUGSCAN_RESOLVERS`, then `~/.config/plugscan/resolvers`. Running
  plugscan inside an untrusted directory never loads that directory's TOMLs.
- **HTTPS-only.** Any non-https `page`/`download`/`changelog`/`login` is
  rejected at resolver load, loudly.
- **Terminal-injection hardening.** Control characters (including ANSI
  escape introducers) are stripped from every string that crosses a trust
  boundary before it reaches the catalog or the terminal.
- **Linear-time regex.** The `regex` crate cannot backtrack, so a malicious
  `version_regex` cannot ReDoS the checker.
- **System TLS.** HTTP uses the OS trust store (Security.framework via
  native-tls), not a bundled CA list.
- **Never executes anything.** plugscan does not run installers, does not
  strip quarantine attributes, and has no auto-update of itself.
- **Local-only.** No accounts, no telemetry; the inventory never leaves the
  machine. Network traffic is exactly: fetches of resolver-listed https URLs.
- **Supply chain.** `cargo deny check licenses advisories` gates dependencies
  (MIT/Apache-2.0 policy + RustSec advisories); see `deny.toml`.

## Known residual risks

- **Compromised vendor site.** If a vendor's own page/download is
  compromised, plugscan will report what the vendor publishes. Mitigations:
  the user's browser and OS Gatekeeper remain in the loop (plugscan never
  installs); the future `fetch` phase will record checksums and keep
  quarantine attributes intact.
- **Redirect downgrade.** A https fetch that redirects is followed by the
  HTTP client; a hostile redirect chain could serve attacker content for
  version parsing. Impact is limited to a spoofed version string. The fetch
  phase must re-verify scheme and host after redirects before saving files.
- **Stale/lying vendor pages** produce wrong versions, not code execution.
  The staleness rule (never report a "latest" older than the installed
  version) and per-resolver verification keep this visible.

## Rules for the community resolver repo

Every resolver PR is validated by CI before human review:

1. TOML schema check; `version_regex` must compile with exactly one capture.
2. https-only; no IP-literal hosts; no URL shorteners.
3. Live exercise: fetch the page, run the regex, require a plausible version.
4. **Domain-change review:** a diff that changes an existing vendor's
   `download`/`page` host is flagged for extra scrutiny — that is the
   malicious-PR shape.
5. Merge requires a maintainer review; the branch is protected.

Agent-curated PRs follow the same pipeline. Agents that read vendor pages
treat page content strictly as data; nothing a page says can change what the
validators enforce, and nothing lands without the CI gate plus review.

## Reporting

Open a security advisory or contact the maintainer privately; do not file
public issues for exploitable problems.
