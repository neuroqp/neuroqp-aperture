# Corresponding-source preparation

Keep the existing archive coverage and integrity gate. Native libraries, Cargo/npm
dependencies, patches, build recipes and notices remain required. A faster release
must not reuse the 0.1.1 application snapshot for different application source or
pretend that its old lockfile covers a changed dependency set.

## One assembly after source freeze

Use `refresh-source.py` with the original frozen-attempt receipt, exact frozen
commit, and a verified previous archive/review. It reuses reviewed non-application
members when native/dependency inputs match, creates the small application
snapshot from exact Git blobs, and writes one refreshed archive. Dependency
changes that the reuse recipe cannot establish require separate source review.

The external review initially says `assembled-awaiting-review`. Review archive
coverage, exact app/member bytes, native/lock identities and checksum before
marking that external manifest ready. Keep this manifest outside the frozen Git
commit. Pin its exact file SHA-256. The original deadline covers assembly and review;
a timeout preserves diagnostic output and blocks publication.

```sh
python3 scripts/release/refresh-source.py --help
python3 scripts/release/release.py source-material --help
```

The refresh command requires `--source-sha`, `--prior-archive`, `--prior-review`,
`--output` and, for an actual write, `--attempt` and `--write`. The external source
gate accepts `--plan PATH --plan-sha256 SHA256 --source-sha FROZEN_SHA --attempt ORIGINAL_RECEIPT`. It checks the
manifest hash before parsing and verifies the archived application inventory and
actual members against that frozen Git tree. Only the existing publication JSON
is excluded; missing, changed, duplicate, extra or unsafe members fail.

`bounded.py source-receipt ORIGINAL_RECEIPT REVIEW.json --plan-sha256 SHA256`
also checks the assembly proof against the original freeze and deadline. Preserve
the original receipt bytes. Reformatting or recreating that file changes its identity.

After an orchestration-only stop, reuse an already reviewed archive only when the
frozen application commit, archive hash, native inputs and locks are unchanged.
In the new bounded attempt, rerun full
`verify_source_material(root=ROOT, source_sha=FROZEN_SHA)` verification, including
the archived Git inventory, then approve a new external manifest. Its legacy
`assembly_attempt` field records the new freeze identity, timestamps and passing
verification result with `kind: verified-reuse`, `archive_written: false` and
`verification_receipt_sha256`. Preserve `original_assembly_attempt` unchanged and
`original_review_sha256`; keep `refresh_receipt_sha256` bound to the original
`REFRESH` proof. This records fresh verification without recompressing bytes or
claiming another assembly.

## Development runtime coverage

The reviewed `0.2.0` development baseline is `source-v0.2.0-r3`, pinned by the
tracked `corresponding-source.json`. It retains 522 Cargo crates after removal of
the single-instance plugin, all 60 npm archives and the reviewed native sources.
Its application snapshot predates the cache-publication fix and Stage A/B/C
workflow changes. Development coverage therefore identifies the current public
Git commit separately; Stage B refreshes the exact frozen application snapshot.

Stage A uses `release.py development-source` before uploading development runtimes.
It verifies the immutable baseline archive once, actual native BUILDINFO and notices,
and the current public Git source. It emits a small `DEVELOPMENT-SOURCE.json` into
the runtime archive instead of uploading the baseline sources again. Only a proven
Cargo root-package version change is allowed against the baseline lockfile; changed
dependencies or native inputs require separate source review. This coverage record
does not replace Stage B's exact frozen-source archive gate.

Publish the once-assembled, reviewed bytes under a new source-only tag outside
Latest. Transfer those same bytes to Stage B. Do not rebuild on a runner to upload
them. The candidate source job downloads/validates the published archive once and
passes its artifact to the package jobs. Existing exact-checksum archives and
`--cache PATH` entries are reused; checksum mismatch requires a verified fetch.
Lock mismatches fail before network access. Each actual BUILDINFO still has to
match the frozen source, lockfiles and native dependencies.

## Remaining size limitation

The current schema is still a monolithic archive of roughly 1.7 GiB. Reusing
reviewed dependency members avoids another dependency audit, but the refreshed
archive still contains them. This PR removes redundant assembly/download/copy
work; it does not claim that each new application snapshot uploads only a few MiB.

A separate immutable dependency archive plus a small application snapshot needs a
new composition manifest, explicit dependency-closure proof and migration of the
source gate and release asset set. That is deferred as a separate change rather
than expanding this process repair. Existing public archives/tags remain usable
and immutable. Until that change, one full source assembly and review can consume
a substantial part of the 30-minute budget; an overrun blocks the attempt.

## Historical 0.1.1 review

The corresponding-source archive preserves original upstream sources, patches,
build recipes, license material and application dependencies for Aperture 0.1.1.
Its first `SOURCE-MANIFEST.json` pins the platform inventories and native identities.
The companion `corresponding-source.json` records publication readiness and the
archive checksum, exact application lockfile hashes and the archived application
source-inventory hash. Inventory assembly labels inside the archive are historical
records; they do not replace that reviewed publication record.

The `0.1.1` material refreshes the application snapshot and Cargo root version
after the native window-close permission fix. Native/vendor sources and npm
dependency resolutions are unchanged. The `source-v0.1.0` archive and tag remain
immutable; refreshed material uses the separate `source-v0.1.1` identity.

The first manifest pins `application/source-inventory.json`. The reviewed plan
records that inventory's hash and the hashes of its actual Cargo/pnpm lockfile
bytes. Full archive readback compares the application snapshot with its committed
Git tree and verifies the dependency inventories. Candidates carry the current
reviewed plan, while the archived assembly labels remain historical records.
Preflight rejects changed checkout locks before downloading; upload and metadata
gates require each actual BUILDINFO's exact lock hashes as well as native identities.
The gate also reads the bounded archived application source inventory, verifies
its pinned content hash, and compares its Cargo/pnpm entries against the plan.
Changing the plan and checkout locks together cannot reuse an old archive.
LF checkout attributes preserve these exact bytes on Windows and Linux. Refreshed
material still requires review of the actual archived lockfile bytes.

The separate unsigned `0.0.9` Windows upgrade fixture uses an explicit fixture gate.
It preserves its replacement Cargo lock under `test-deltas` and must match the
reviewed `0.1.1` production lock byte for byte except the aperture root package
version. Other candidate versions require a separately reviewed acceptance fixture.
The fixture BUILDINFO must hash that exact replacement; pnpm and all dependency
entries stay pinned. Ordinary uploads and aggregate release metadata accept only
the reviewed production lock hashes.

Windows archive inputs are selected from the verified inventory plus explicit
metadata files. Unpacked audit scratch remains excluded. The verifier checks
those publication inputs, and archive assembly preserves that selection.

The Windows review covered 42 native executable/library files and all 28 vendor
component versions, recipe sources and patches, the librsvg locked crate closure,
GVDB, compiler runtimes, NSIS and its Tauri installer plugin. The vendor does not
attest which historical OCI base produced its ZIP. The selected base is supported
by chronology and preserved source/version evidence. The permissively licensed
Tauri NSIS plugin also has no published historical Cargo lock or compiler identity;
its declared dependencies, source and license evidence are preserved without
inventing a historical resolution.

The Linux review mapped the AppImage's 216 ELF records to their source/build
material. It includes 103 exact Debian runtime source packages, 236 rav1e build
dependency sources, libvips and AppImage launcher/runtime sources, and the embedded
runtime's static dependency sources and patches. Original package hashes, build
IDs, signed repository indexes and build records support the mappings. The
retained Linux BUILDINFO identifies the audit baseline, not a new candidate.

Application material includes all 523 locked Cargo registry crates and 27,007
verified vendor file hashes, all 60 locked npm archives with registry integrity
checks, local path dependencies and the application source snapshot. A fresh
offline/frozen pnpm install and offline/locked Cargo metadata resolution passed.
Pinned Rust standard-library copyright and license reports are included in both
platform notices. Compiler and proprietary OS SDK prerequisites remain build
prerequisites; this is not a mirror of those complete toolchains.

Independent checks verified the platform inventories, source mappings, patch
application records, dependency closure and archive writer. Whole-package offline
or byte-identical rebuilds have not been demonstrated. Windows expected native
metadata and the retained Linux baseline must match each actual candidate's
BUILDINFO before any binary upload. A dependency change requires refreshed source
material and review. Stage A functional validation, Stage C package smoke and recoverable updater-key
backup are separate requirements. The source review does not establish them.
