# Security scanning

**Security audit** runs daily at 07:41 UTC, on pull requests that change
dependency manifests, lockfiles or the audit setup, and through **Run workflow**.

- JavaScript: pnpm 9.15.9 audits `pnpm-lock.yaml` without installing packages.
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

**Dependabot** opens grouped security-fix PRs for npm, Cargo and GitHub
Actions. Ordinary version-update PRs stay off. Nothing merges or deploys
automatically.

Run the audits locally without changing dependencies:

```sh
node scripts/security-audit.mjs --self-test
node scripts/security-audit.mjs /tmp/aperture-security-audit
cargo audit --file src-tauri/Cargo.lock
```

Not covered: native runtimes outside the lockfiles. That includes libvips and
its image libraries, WebKitGTK and system libraries in Linux packages, and the
WebView2 runtime and native readers bundled in Windows packages. A clean audit
says nothing about them.
