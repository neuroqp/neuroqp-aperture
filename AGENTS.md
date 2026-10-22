# Development

- Follow `SPECS.md`. Keep changes minimal, readable and focused on current needs.
- Format frontend files with Prettier and Rust with `cargo fmt --all`.
- Test meaningful behavior at the appropriate level. Validate native desktop
  dialogs, saving and lifecycle in the running application.
- Keep real microscopy datasets, generated caches and validation artifacts out of
  Git. Never modify source test images.

# Native GUI tests

- Run native scripts, GUI checks and computer-use validation headless by default:
  `scripts/headless-session [--driver PORT] -- COMMAND`. Take screenshots with
  `grim` inside that session; never on the user's display.
- Never send `ydotool`, `wtype`, `hyprctl` input, focus or clipboard changes to
  the user's desktop. Drive windows through `scripts/compositor.py`.
- Use `scripts/visible-session` only when the user asks or a problem cannot be
  reproduced headless. See `VALIDATION.md`.

# Releases

- Follow `RELEASING.md`: validate, package, publish.
- Do full functional testing in feature PRs. A release freezes scope and reuses
  accepted evidence by exact hashes. It adds no feature test matrices, harnesses,
  capture tooling or repeated review loops, and does not rerun functional suites.
  CI regressions and build, identity, source and cryptographic checks remain.
- Finish fixes, corresponding-source preparation and desktop preparation before
  packaging. Package the exact validated executables and runtime dependencies
  without recompilation or fresh dependency resolution.
- After packaging, verify hashes, updater signatures and source coverage. Run only
  the four launch, small-TIFF render and normal-close package smokes on the exact
  delivered bytes, then publish those bytes.
- Give each smoke one attempt and at most one bounded retry for an obvious setup
  issue. An application failure blocks publication. An automation failure or
  unavailable OS leaves that target unverified with a recorded follow-up; never
  report it as a pass. See `RELEASING.md` for the current four-report limit.
- One operator owns orchestration. Give subagents explicit ownership; they must
  not independently start builds or VMs.
- Preserve completed outputs and failed diagnostics. Per-step timeouts stop that
  operation; elapsed time does not invalidate unchanged source or artifact hashes.
  Resume from the failed operation after repair, rerunning affected checks only.
- Authorization to complete a release covers its repairs, retries and publication.
  Do not add recurring permission gates. Respect any narrower user instruction.
- Never move a published tag or replace different public asset bytes. A defect in
  a public application release requires a higher version.

# Git safety

- Create or reuse a feature branch. From `origin/main`, use
  `git switch --no-track -c <branch> origin/main`.
- Never push `main` or let another branch track `origin/main`.
- Push the feature branch and open a ready PR to `main`. Merge only when the user
  has explicitly authorized it.
