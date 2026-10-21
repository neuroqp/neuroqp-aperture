# Security scanning

**Security audit** runs daily at 07:41 UTC, on pull requests that change
dependency manifests, lockfiles or the audit setup, and through **Run workflow**.

- JavaScript: pnpm 10.34.7 audits `pnpm-lock.yaml` without installing packages.
  Builds keep pnpm 9; only the audit uses pnpm 10, because earlier versions
  call only the legacy registry endpoint, which can fail for vulnerable trees.
  High/critical production findings fail, including findings with no published
  fix. Development findings and lower severities stay visible without failing.
- Rust: cargo-audit 0.22.2 (checksum-pinned release binary) audits
  `src-tauri/Cargo.lock` against the RustSec database. Any vulnerability fails.
  Unmaintained, unsound and yanked warnings stay visible without failing.
- Registry, advisory database and tool errors fail the check too. JSON
  reports, stderr and a readable summary are kept as workflow artifacts for 30
  days.

**CodeQL** analyzes GitHub Actions workflows, TypeScript, release Python
scripts and Rust daily at 08:19 UTC, on pull requests and on pushes to `main`.
It uses `build-mode: none`, so it needs no native packages or release build.
Use this workflow instead of CodeQL default setup; GitHub rejects uploads when
both are enabled.

**Dependabot** opens grouped security-fix PRs for npm and Cargo; ordinary
version updates stay off for them. Workflows pin GitHub Actions to commit
SHAs, which Dependabot alerts do not cover, so Actions get one grouped
version-update PR per week instead, for releases at least 7 days old. Review
those like any dependency change. Nothing merges or deploys automatically.

Run the audits locally without changing dependencies. The script uses the
`pnpm` on `PATH`; use pnpm 10.30.1 or newer so vulnerable trees report.

```sh
node scripts/security-audit.mjs --self-test
node scripts/security-audit.mjs /tmp/aperture-security-audit
cargo audit --file src-tauri/Cargo.lock
```

Not covered: native runtimes outside the lockfiles. That includes libvips and
its image libraries, WebKitGTK and system libraries in Linux packages, and the
WebView2 runtime and native readers bundled in Windows packages. A clean audit
says nothing about them.
