# Development

- Follow SPECS.md. Keep the implementation minimal, readable, and focused on
  current requirements. Apply KISS and YAGNI.
- Format frontend files with Prettier and Rust with `cargo fmt --all`.
- Test meaningful behavior at the appropriate level. Validate native desktop
  flows in the running application; browser previews alone are insufficient.
- Keep real microscopy datasets, generated caches, and validation artifacts out
  of Git. Never modify source test images.

# Release contract

- Follow `RELEASING.md`. Complete functional testing and fixes before final OS
  packaging. Use CI-built executables for limited native desktop checks.
- Require successful readiness evidence for the exact reviewed source,
  executable bytes, runtime dependencies, source coverage and prepared smoke labs.
  Missing evidence or unsupported tooling blocks deployment before packaging.
- Package and sign the approved executables and dependencies. Do not recompile
  application code or resolve fresh dependencies during release execution.
- After packaging, run only the fixed package smoke checks, signature/hash/feed
  validation, then publish the exact tested bytes. Do not expand acceptance testing.
- One agent owns release execution and gives every subagent this contract and
  explicit ownership. Subagents must not independently launch builds or VMs.
- Use one original release deadline. A failed readiness check or required release
  step stops the current deployment request: preserve evidence and report the
  blocker. Do not automatically create repair PRs, rebuild, reset the freeze clock
  or redispatch. Only the documented bounded transient smoke retry is permitted.
- Routine deployment must not expand into CI implementation, dependency/source
  repairs, VM provisioning or automation repair. Those require separately scoped
  development work; calling them Stage A does not authorize another release loop.

# Git safety

- Create or reuse a feature branch. From `origin/main`, use
  `git switch --no-track -c <branch> origin/main`.
- Never push `main` or let another branch track `origin/main`.
- Push the feature branch and open a ready PR to `main`. Never merge it.
