# Development

- Follow SPECS.md. Keep the implementation minimal, readable, and focused on
  current requirements. Apply KISS and YAGNI.
- Format frontend files with Prettier and Rust with `cargo fmt --all`.
- Test meaningful behavior at the appropriate level. Validate native desktop
  flows in the running application; browser previews alone are insufficient.
- Keep real microscopy datasets, generated caches, and validation artifacts out
  of Git. Never modify source test images.

# Git safety

- Create or reuse a feature branch. From `origin/main`, use
  `git switch --no-track -c <branch> origin/main`.
- Never push `main` or let another branch track `origin/main`.
- Push the feature branch and open a ready PR to `main`. Never merge it.
