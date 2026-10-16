# Corresponding source

Prepare corresponding source during validation, before packaging. Keep complete
coverage for the shipped native libraries, Cargo/npm dependencies, patches,
build recipes and notices. Source links and notices alone do not replace this
material. A dependency or runtime identity mismatch blocks binary publication.

## Reusable dependencies and exact application source

Each release distributes a small `Aperture-Application-Source.tar.xz` and an
`Aperture-Corresponding-Source.json` composition manifest. The manifest pins the
application archive and an existing immutable dependency-source archive by URL
and SHA-256. This reuses reviewed upstream material without uploading the entire
dependency archive for each application change. Keep the referenced archive
public and immutable.

The application archive contains the exact source commit, current build recipes
and lockfiles. The source gate verifies actual Cargo dependency closure against
the baseline vendor inventory, exact npm lock identity and native identities.
A changed root application version does not imply changed dependency coverage.
New or changed dependencies require refreshed reviewed material before packaging.

Release helpers require Python 3.11 or newer. On this host use `python3.12`,
because its unqualified `python3` resolves to 3.10. Prepare and verify the files
in an owned output directory:

```sh
python3.12 scripts/release/release.py source-material OUTPUT_DIRECTORY --source-sha FULL_SHA --fetch --cache CACHE_DIRECTORY
```

Use `--help` for optional cache and input arguments. Preserve the generated
composition, application archive and their hashes with validation evidence.
Validation performs complete source coverage and archive-member verification.
Package and publish consume these files with cheap manifest/archive hash checks;
they must not repeat the source audit, assemble another archive or fetch a
different dependency resolution.

## Required identities

The composition and actual BUILDINFO must agree on application commit, Cargo/pnpm
lockfiles and native dependency versions. Verify archive members and application
Git inventory, rejecting changed, missing, duplicate, extra or unsafe members.
Verify coverage against the actual runtime payload rather than a package name or
an unrelated historical BUILDINFO. Keep hashes for the manifest and its referenced
archives. A cached file is reusable only when its checksum matches.

The reviewed immutable baseline remains pinned in `corresponding-source.json`.
It preserves the upstream native/vendor sources, patches, recipes and notices
from the original review. Its application snapshot is historical; the new
application archive supplies current application source and recipes. To assemble
the complete source tree, extract the immutable dependency bundle, remove its
historical `application/source` tree entirely, then extract the current
application archive in its place. Use the current recipes and lockfiles. Do not
claim whole-package offline or byte-identical rebuilds without demonstrating them.

Elapsed time does not expire an unchanged reviewed archive. Revalidate relevant
identities after a repair, retain failed verification output, and reuse matching
bytes. Never replace a published source archive or move its tag. Keep signing
keys out of all source archives, reports, logs and Git.
