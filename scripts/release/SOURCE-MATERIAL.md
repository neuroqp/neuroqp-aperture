# First-release source material review

The corresponding-source archive preserves original upstream sources, patches,
build recipes, license material and application dependencies for Aperture 0.1.0.
Its first `SOURCE-MANIFEST.json` pins the platform inventories and native identities.
The companion `corresponding-source.json` records publication readiness and the
archive checksum. Inventory assembly labels inside the archive are historical
records; they do not replace that reviewed publication record.

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
