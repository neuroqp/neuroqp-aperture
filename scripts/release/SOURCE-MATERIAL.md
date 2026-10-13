# First-release source material review

The corresponding-source archive preserves original upstream sources, patches,
build recipes, license material and application dependencies for Aperture 0.1.0.
Its first `SOURCE-MANIFEST.json` pins the platform inventories and native identities.
The companion `corresponding-source.json` records publication readiness and the
archive checksum, exact application lockfile hashes and the archived application
source-inventory hash. Inventory assembly labels inside the archive are historical
records; they do not replace that reviewed publication record.

The first-release lock binding was checked against the actual archived lockfile
bytes and their entries in `application/source-inventory.json`. The first manifest
pins that inventory as
`f2810834ef5f8b3973c5f396933f7d3613d52934f7c4a64f9b16e3aab2a36dc5`.
The Cargo lock hash is
`4a5bd85ee8786b2e23ba216b03d75e628de6b35cccbcb0304f74c5d9f62a8bc7`;
the pnpm lock hash is
`ce391502197571c39a3558c88de3d7c701c91f9175404e9718c84b3e62e53f84`.
Both match the release checkout. The archive remains immutable; its original
companion is historical, while candidates carry the current reviewed plan.
Preflight rejects changed checkout locks before downloading; upload and metadata
gates require each actual BUILDINFO's exact lock hashes as well as native identities.
The gate also reads the bounded archived application source inventory, verifies
its pinned content hash, and compares its Cargo/pnpm entries against the plan.
Changing the plan and checkout locks together cannot reuse an old archive.
LF checkout attributes preserve these exact bytes on Windows and Linux. Refreshed
material still requires review of the actual archived lockfile bytes.

The separate unsigned `0.0.9` Windows upgrade fixture uses an explicit fixture gate.
It preserves its replacement Cargo lock under `test-deltas` and must match the
reviewed `0.1.0` production lock byte for byte except the aperture root package
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
