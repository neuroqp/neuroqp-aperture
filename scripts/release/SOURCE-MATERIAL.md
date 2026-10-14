# Corresponding-source material review

## Aperture 0.2.0 refresh

The `source-v0.2.0` archive refreshes the application snapshot and Cargo root
version from the reviewed `0.1.1` material. The removed single-instance plugin
reduces the Cargo registry closure to 522 crates and 26,992 vendor files. All
60 npm archives and the Windows/Linux native sources, patches and build inputs
remain unchanged. Original source releases remain immutable.

`refresh-source.py` takes a full committed application SHA, checks the prior
reviewed archive and native build inputs, and rejects new dependency resolutions.
It records a pending publication plan, then reads the complete new archive back
to verify every retained member and generated application file. Review that
receipt and dependency closure before changing the plan to ready and publishing.
The final publication plan is excluded from its own application inventory to
avoid a checksum cycle. Binary uploads still require exact application lock and
native dependency identities from each actual build.

Previous-stable upgrade fixtures retain the released `0.1.1` application source,
version, locks and verification key. Their loopback transport overlay is separate
QA material, checked through the ordinary prior-source gate. The historical
`0.0.9` bootstrap exception below does not apply to those fixtures.

## Retained 0.1.1 review

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
material and review. Desktop acceptance and updater-key backup are separate
release requirements.
