# Audio Plugin Manifest (APM) — Specification v1.0 (draft)

**Status:** draft for discussion · **Editor:** the plugscan project · **License:** CC0 (the spec text is public domain; adopt freely)

## 1. Why this exists

Update-awareness tools for audio plugins today work by *reverse-engineering*
each manufacturer's website: scraping changelog pages, guessing which number on
a download page is "the latest," matching plugins to products by name, and
special-casing every inconsistency. This is fragile and unbounded in
maintenance cost — a vendor redesigns a page and every tool breaks silently.

The information these tools need is not secret. **The manufacturer already
knows it.** They simply have no standard, machine-readable way to *publish* it.

APM is that format: a small JSON document a manufacturer hosts, declaring — for
every product and plugin they ship — the current version on each release
channel, where to download it, how to verify it, and which plugin binaries it
corresponds to. Any tool can consume it the same way. No scraping, no guessing.

Every field in this spec traces to a concrete failure observed while
maintaining bespoke resolvers against a ~5,300-plugin library. Those cases are
cited inline as **[case: …]** so the rationale is never abstract.

## 2. Design principles

1. **Publisher-declared, not consumer-inferred.** Anything a tool currently
   guesses (is this a beta? is this the paid tier? which platform?) becomes an
   explicit field the publisher owns.
2. **Identity by stable machine IDs, never by name.** Plugins are matched by
   their AU/VST3/CLAP identifiers, which are already baked into every binary.
   Human names are display-only. **[case: matching `"Benson Delay"` against a
   regex-escaped `productName` was the single most fragile part of every
   resolver.]**
3. **Decentralized discovery.** No central registry to run or trust. A plugin
   points at its own manifest; a domain exposes one well-known location.
4. **The manifest is the source of truth for "latest," and can be for
   "installed" too.** Published per-artifact checksums let a tool determine the
   *true* installed version by hashing the binary — even when the bundle's own
   embedded version string is wrong. **[case: Ignite Amps shipped Libra 1.3.0
   with an `Info.plist` still declaring 1.2.0; only the install receipt was
   truthful.]**
5. **Cheap to adopt.** A valid manifest for a one-plugin product is a few lines.
   Complexity (channels, collections, entitlements) is opt-in.

## 3. Discovery

A consumer locates a manifest by either mechanism; the first that resolves
wins. Both are decentralized — the manifest is always hosted by the party that
ships the software.

### 3.1 Bundle-embedded pointer (primary)

Each plugin bundle's `Contents/Info.plist` MAY contain a string key:

```
APMManifestURL = "https://vendor.example/.well-known/audio-plugins.json"
```

A consumer reads this while it inventories the bundle (it is already parsing
`Info.plist`), so no name-based lookup is ever needed: the installed artifact
itself says where its manifest lives. This is the preferred mechanism because
it is exact and survives the vendor reorganizing their website.

### 3.2 Well-known location (fallback)

A publisher SHOULD also serve the manifest at the RFC 8615 well-known path on
their primary domain:

```
https://vendor.example/.well-known/audio-plugins.json
```

This lets a consumer that knows only the vendor's domain (e.g. from the
`publisher.homepage` of another data source) find the manifest without an
embedded pointer, and gives every publisher one canonical URL.

### 3.3 Serving requirements

- `Content-Type: application/json; charset=utf-8`.
- Served over HTTPS. HTTP redirects to the canonical HTTPS URL are permitted.
- CORS: `Access-Control-Allow-Origin: *` SHOULD be set so browser-based tools
  can read it.
- The document SHOULD be cacheable (`Cache-Control`, `ETag`); consumers SHOULD
  honor caching and SHOULD NOT poll more than once per hour per manifest.

## 4. The manifest document

A single JSON object.

### 4.1 Top level

| Field | Type | Req | Meaning |
|---|---|---|---|
| `apm_version` | string | ✓ | Spec version this document targets, e.g. `"1.0"`. |
| `publisher` | object | ✓ | See §4.2. |
| `products` | array&lt;Product&gt; | ✓ | One entry per sellable/installable product. |
| `updated` | string (date) | – | ISO-8601 date the manifest was last changed. |

### 4.2 `publisher`

| Field | Type | Req | Meaning |
|---|---|---|---|
| `name` | string | ✓ | The manufacturer's display name. |
| `homepage` | string (URL) | ✓ | Primary site. |
| `distributor` | string | – | Party that actually hosts installers/support, when different from the maker. **[case: Libra is made by Ignite Amps but distributed by STL Tones; Newfangled Audio is distributed through Eventide.]** |
| `support` | string (URL) | – | Support/contact URL. |

### 4.3 Product

A **product** is one thing a user installs. Its installer MAY deliver several
plugin binaries (a "collection"); it MAY deliver exactly one. The product
carries a single version per channel — the version of *the installer*.

| Field | Type | Req | Meaning |
|---|---|---|---|
| `id` | string | ✓ | Stable, publisher-assigned slug, unique within the manifest (e.g. `"benson-chimera-collection"`). |
| `name` | string | ✓ | Display name. |
| `plugins` | array&lt;Plugin&gt; | ✓ | The plugin binaries this product installs (§4.4). A collection lists all of them. **[case: MixWave's "Benson Chimera Collection" installer delivers six pedals and stamps them all with the collection's version; their per-pedal standalone version fields were stale and caused a reported "latest" *below* what was installed.]** |
| `channels` | object&lt;name→Channel&gt; | ✓ | Release channels (§4.5). MUST include `stable`. |
| `entitlement` | object | – | Paid-upgrade boundary (§4.7). |
| `homepage` | string (URL) | – | Product page. |

### 4.4 Plugin and identity

A **plugin** is a single loadable binary. It is matched to an installed file by
its format identifiers, which are intrinsic to the binary and never change
across a rename or a website redesign.

| Field | Type | Req | Meaning |
|---|---|---|---|
| `name` | string | – | Display name (informational). |
| `formats` | array&lt;string&gt; | ✓ | Any of `"AU"`, `"VST3"`, `"VST2"`, `"CLAP"`, `"AAX"`. |
| `identifiers` | object | ✓ | At least one stable ID (below). |

`identifiers` keys — provide every one that applies:

| Key | Type | Source of truth |
|---|---|---|
| `au` | string | AudioUnit `type:subtype:manufacturer`, each a four-char code, e.g. `"aufx:Lbr1:IgnA"`. |
| `vst3` | string | VST3 component class ID (the 128-bit `FUID`, 32 hex chars). |
| `vst2` | string | VST2 four-char plugin unique ID, e.g. `"LbrA"`. |
| `clap` | string | CLAP plugin id (reverse-DNS), e.g. `"com.igniteamps.libra"`. |

Matching is exact on any shared identifier. Names are never used for matching.

### 4.5 Channel and release

A **channel** is a named release stream. `stable` is required; publishers MAY
add any others (`beta`, `rc`, `nightly`, `early-access`, …). A channel's
`current` release is what a consumer compares against; `history` is optional.

```jsonc
"channels": {
  "stable": { "current": { /* Release */ }, "history": [ /* Release… */ ] },
  "beta":   { "current": { /* Release */ } }
}
```

**Release object:**

| Field | Type | Req | Meaning |
|---|---|---|---|
| `version` | string | ✓ | The authoritative version. Dotted numeric; MAY carry a pre-release suffix. This is the source of truth even if a bundle's embedded version disagrees. **[case: Libra.]** |
| `stability` | string | ✓ | `"released"`, `"rc"`, `"beta"`, `"alpha"`, or `"dev"` — **declared, not inferred from the version string**. **[case: BEATSURFING tags *every* build `-rc`, including shipped stable ones, so a suffix-sniffing heuristic is worthless; only the publisher knows a build's real stability.]** |
| `released` | string (date) | – | ISO-8601 release date. |
| `notes` | string (URL) | – | Changelog/release-notes URL. |
| `platforms` | object&lt;name→Platform&gt; | ✓ | Per-OS artifacts (§4.6). |

### 4.6 Platform and artifact

Keyed by OS: `"macos"`, `"windows"`, `"linux"`.

| Field | Type | Req | Meaning |
|---|---|---|---|
| `url` | string (URL) | ✓ | Direct, unauthenticated download of the installer/archive where possible. If the download requires login, omit `url` and set `download_page`. |
| `download_page` | string (URL) | – | Human landing page when a direct URL cannot be public. |
| `sha256` | string | – | Hex SHA-256 of the artifact. Enables verified download **and** authoritative installed-version detection (§5.2). Strongly recommended. |
| `size` | integer | – | Bytes. |
| `min_os` | string | – | Minimum OS version, e.g. `"11.0"`. |
| `arch` | array&lt;string&gt; | – | e.g. `["arm64","x86_64"]`. |

Per-OS separation removes a whole class of confusion. **[case: LiquidSonics'
Windows and macOS builds carry different version numbers; a single "latest"
field cannot represent both.]**

### 4.7 `entitlement`

Expresses the free/paid boundary so a consumer never mislabels a paid major
upgrade as a free update.

| Field | Type | Meaning |
|---|---|---|
| `free_through` | string | Highest version a current owner gets for free (e.g. `"1.x"` or `"1.9.9"`). |
| `paid_from` | string | First version requiring a paid upgrade (e.g. `"2.0"`). |

**[case: Synchro Arts RePitch — 1.x owners must pay for 2.0; reporting 2.0 as a
plain update is wrong.]**

## 5. Version semantics

### 5.1 Comparison

Versions are compared component-wise on their numeric parts (split on any
non-digit run), missing trailing components treated as zero. Pre-release
suffixes do **not** affect the numeric comparison; a release's channel and
`stability` field — not its version string — decide whether it is a stable
update. Consumers MUST NOT present a release from a non-`stable` channel as a
stable update.

A consumer MUST NOT present a `current.version` that is **lower** than the
user's installed version as an update. **[case: MixWave — this is exactly the
"latest below installed" symptom that flags a mis-modeled product.]**

### 5.2 Determining the installed version

A consumer must decide *which version the user currently has* before it can say
whether an update exists. This is a consumer design choice, not a manifest
requirement; the spec supports more than one approach and mandates none.

**Option A — trust the bundle's embedded version (default).** Read the
plugin's `CFBundleShortVersionString` (or platform equivalent). Zero extra
cost, works with any manifest. Its weakness is that the embedded string is
occasionally wrong — a vendor ships a new build without bumping it. **[case:
Ignite Amps shipped Libra 1.3.0 with an `Info.plist` still saying 1.2.0.]**

**Option B — checksum match against published artifacts (design option).**
When a release publishes per-artifact `sha256`, a consumer MAY hash the
installed binary (or a stable component of it) and match it against the
manifest's checksums. A match proves exactly which release is installed,
independent of any embedded string, and would retire both plist-trust and
OS-install-receipt workarounds. The tradeoffs a consumer weighs: it costs a
hash over each bundle during scan; it only resolves versions the manifest still
lists (a very old install may match nothing); and it depends on the publisher
providing checksums, which §4.6 recommends but does not require. Offering it as
an opt-in refinement over Option A — rather than a default — keeps scans cheap
while making authoritative detection available where it matters. **[case:
Libra, resolvable this way without touching `/var/db/receipts`.]**

Because Option B is optional on both sides (publisher may omit `sha256`,
consumer may skip hashing), `sha256` is *recommended, not required* in §4.6:
its primary guaranteed use is verified download, with installed-version
identification as an additional payoff where present.

> **To be discussed with plugin vendors.** Whether checksum-based installed
> detection (Option B) is worth the publishing effort — and *what* gets hashed
> (whole bundle, main binary, a designated component) so a consumer and a
> publisher always agree — is an open design point to settle with vendors, not
> a decision this draft fixes. See the appendix.

## 6. Consumer behavior

A conforming consumer, given an installed plugin:

1. Read `APMManifestURL` from the bundle's `Info.plist`; if absent, try the
   vendor domain's `.well-known/audio-plugins.json`.
2. Fetch and parse; reject documents whose `apm_version` major it does not
   support.
3. Match the installed binary to a `Plugin` by shared identifier (§4.4).
4. Read the enclosing product's `stable` channel `current.version` as "latest
   stable"; read other channels as separate, clearly-labeled streams.
5. Compare with the installed version (§5.1). Never report a lower version as
   an update; honor `entitlement` before labeling an update "free."
6. If verifying/downloading, use the matching `platform` artifact and check
   `sha256`.

## 7. Worked example

```jsonc
{
  "apm_version": "1.0",
  "updated": "2026-09-04",
  "publisher": {
    "name": "Ignite Amps",
    "homepage": "https://www.igniteamps.com",
    "distributor": "STL Tones",
    "support": "https://www.stltones.com/pages/support"
  },
  "products": [
    {
      "id": "libra",
      "name": "Libra",
      "plugins": [
        {
          "name": "Libra",
          "formats": ["AU", "VST3", "AAX"],
          "identifiers": {
            "au": "aufx:Lbr1:IgnA",
            "vst3": "56535449-4C62-7231-49676E41-00000000",
            "clap": "com.igniteamps.libra"
          }
        }
      ],
      "channels": {
        "stable": {
          "current": {
            "version": "1.3.0",
            "stability": "released",
            "released": "2026-07-20",
            "notes": "https://www.stltones.com/pages/libra-changelog",
            "platforms": {
              "macos": {
                "url": "https://cdn.stltones.com/libra/Libra-1.3.0-mac.pkg",
                "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "min_os": "11.0",
                "arch": ["arm64", "x86_64"]
              }
            }
          }
        }
      }
    }
  ]
}
```

A collection product (MixWave-style) differs only in listing several entries in
`plugins[]` under one product with one version.

## 8. Compatibility & migration

- **Sparkle projection.** A publisher already serving a Sparkle appcast can be
  supported by a small adapter that projects APM → appcast per product, so
  Sparkle-based hosts interoperate for free. APM is a superset of what an
  appcast expresses for a single stream.
- **Resolver bridge.** Tools with existing bespoke scrapers (like plugscan)
  SHOULD prefer a manifest when discoverable and fall back to their scraper
  otherwise, so adoption is incremental and per-vendor rather than all-or-
  nothing. The maintenance burden shrinks monotonically as vendors adopt.

## 9. Conformance

A **conforming manifest** is a JSON document that validates against the schema
in `docs/apm-schema.json`, has `apm_version` `"1.0"`, at least one product,
each product with a `stable` channel, and each plugin with at least one
identifier.

A **conforming consumer** implements §6, matches only by identifier, and never
(a) presents a non-`stable` release as a stable update or (b) presents a
version below the installed one as an update.

## Appendix: open questions — to discuss with plugin vendors

These are deliberately unresolved in this draft. They are the points to settle
*with the manufacturers who would publish manifests*, since they trade
publishing effort against consumer capability and only adopters can judge that
balance.

- **Checksum-based installed detection (§5.2 Option B).** Is publishing
  per-artifact `sha256` worth it to vendors, and if so what unit is hashed —
  the whole installed bundle, the main binary, or a designated stable component
  — so publisher and consumer always compute the same digest? If vendors won't
  reliably publish checksums, Option B stays a niche refinement and Option A
  (embedded version) remains the norm.
- **Identifier canonicalization.** Exact string forms for VST3 `FUID` (byte
  order, casing) and AU four-char codes need a normative canonical encoding so
  two implementations always agree on a match. Vendors' own tooling emits these
  in varying forms today.
- **Signing.** Should manifests be signable (e.g. detached JWS) so a consumer
  can trust a manifest fetched over a compromised path? Deferred to v1.1;
  `sha256` on artifacts already covers download integrity. Of interest to
  vendors who care about update-channel authenticity.
- **Bundle vs component versioning.** Some suites version the suite but ship
  independently-versioned plugins. v1.0 models one version per product; a
  future `plugins[].version_override` could handle the mixed case if real
  vendor examples demand it.
- **Hosting & discovery burden.** Whether vendors prefer the `.well-known` path,
  the embedded `Info.plist` pointer, or both — and how it fits existing CMS /
  storefront setups (several already emit a JSON download feed today) — is worth
  confirming before treating either mechanism as required.
