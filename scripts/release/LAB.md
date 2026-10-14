# Owned QA lab maintenance

The host lab is `/home/pascal/src/neuroqp-aperture-release/artifacts/first-release`.
Use `lab.py` on this Linux host. It reuses existing tools and images, downloads
nothing, and saves no guest RAM. It does not launch guests or claim release
acceptance. Every mutation requires all owned QEMU and swtpm processes to have
stopped. Launchers must hold `environments/bounded-lab/operation.lock` for their
entire VM lifetime so maintenance cannot race a launch.

## Inventory and exact cleanup

Record inventory outside the lab so the before and after totals cover the same
tree. Allocation totals count each device/inode once. Hardlinks are not extra
storage. Btrfs reflinks and snapshots can share extents, so inode allocation is
not a host free-space measurement. `--btrfs` records extent allocation for images.
It also records whole-tree and environment exclusive plus set-shared extent
totals, which count shared rollback reflinks once for the retained storage budget.

```sh
python3 scripts/release/lab.py --output /tmp/lab-before.json inventory --btrfs
python3 scripts/release/lab.py --output /tmp/lab-cleanup-plan.json plan-cleanup
# Review the exact paths, identities and hardlink aliases in this manifest.
python3 scripts/release/lab.py --output /tmp/lab-cleanup-result.json cleanup /tmp/lab-cleanup-plan.json
python3 scripts/release/lab.py --output /tmp/lab-after.json inventory --btrfs
```

The current cleanup allowlist contains the obsolete Windows 10 disk and both
owned hardlinks of its installer ISO. It never includes Windows 11 disk/TPM/UEFI
state, private credential files, microscopy datasets, release candidates,
previous stable packages, source archives or evidence. Cleanup checks every file
identity before deleting any file. A changed file, symlink, different owner or
active VM blocks it. No recursive or wildcard deletion is supported.

Windows 11 installation media needs a current disk-only boot receipt before
removal. Add `--diskboot-receipt /path/boot.json` to `plan-cleanup`. The receipt
must contain `schema: 1`, `status: "pass"`, `install_media_attached: false` and
`disk_identity` from the actual baseline. Attach the observed boot evidence to
the receipt. An old screenshot or launch intent does not establish a boot.

## Recover, compact and prepare disposable runs

The previous Windows 11 host process ended with SIGTERM. A stopped process does
not establish a clean guest shutdown. Recover the existing licensed desktop
within the separately approved bounded session, then observe guest shutdown and
normal QEMU exit. Preserve the original disk, TPM, UEFI and private credentials.
Never print credential contents. Do not create users, reinstall the OS or
download tools for this maintenance task.

A recovery receipt records `schema: 1`, `licensed: true`,
`guest_shutdown: "graceful"`, `qemu_exited: true`, the exact `disk_identity` and
the observation references. A clean shutdown on a CoW overlay applies to that
overlay, not its backing disk. It cannot certify the original as recovered.
Promotion of a recovered overlay requires a separate verified standalone copy;
the current helper refuses compaction or freezing while backing chains exist.

```sh
python3 scripts/release/lab.py --output /tmp/lab-compacted.json compact /path/recovery.json
python3 scripts/release/lab.py --output /tmp/lab-frozen.json freeze-base /tmp/lab-compacted.json
python3 scripts/release/lab.py --output /tmp/lab-run.json overlay candidate-id
```

Compaction needs the current allocated disk size plus 4 GiB of host free space
for a temporary copy. It uses the existing `qemu-img convert -c`, compares all
guest-visible sectors with `qemu-img compare`, and replaces the original only
after successful comparison and an unchanged original identity. A failure keeps
the original and temporary output for inspection. TPM and UEFI are untouched.
Never compact an image that backs an overlay. A successful compaction receipt
can certify the unchanged logical contents for base registration.

Before the approved recovery boot, preserve an owned rollback checkpoint with
`cp --reflink=always --sparse=auto --preserve=mode,timestamps`, plus copies of TPM
and UEFI state in a mode0700 folder. Mark the rollback disk read-only. That copy
preserves an unclean checkpoint; it is not a verified base. Reflink storage can
grow as the original changes. Retire it only after the original recovered and
the verified compacted replacement is durable, using a separate exact reviewed
owned-file manifest. Leaving a rollback behind can prevent compaction from
reducing retained storage even when the replacement is smaller.

Base registration removes disk write permissions and records its identity.
`overlay` creates a small qcow2 overlay and copies the associated TPM and UEFI
state. No memory snapshot is saved. The run's 4 GiB overlay limit is an operator
budget, not an enforced quota; measure it and stop the bounded run before it
exceeds the budget. A launch must respect the maintenance lock and must not
modify the base. Each run still needs its own native desktop evidence.

Linux Debian, Ubuntu and Fedora live installer ISOs already exist. They are not
prepared persistent desktop bases. Preserve those inputs and the verified Debian
tool rootfs. Report Linux readiness as blocked until a separately scoped base
preparation session completes. Do not reconstruct all guests during a release
attempt.

## Retention and measured storage budget

Retain the current and previous release inputs, each verified immutable base,
its TPM/UEFI state, and compact durable evidence containing artifact hashes,
candidate identity, commands, exit codes and representative native screenshots.
After the process is stopped, inspect a completed run's overlay and disposable
caches, record exact owned paths in a reviewed manifest, then retire them.
Do not keep one full lab per attempt. Never delete keys, user-owned disks or
source images, and do not treat duplicate names as duplicate contents. Archive
aliases and reflinks must be accounted for before claiming reclaimed storage.
The present helper intentionally automates only its exact obsolete-file
allowlist; other retention cleanup needs its own reviewed manifest.

The 2026-10-14 pre-cleanup measurement is 90.020 GiB for the whole owned tree and
76.839 GiB for `environments`. The largest consumers are Windows 11 disk
40.549 GiB, obsolete Windows 10 disk plus ISO 14.781 GiB, Windows 11 installer
7.660 GiB and Linux ISOs 11.434 GiB. Windows 10 cleanup alone cannot achieve the
40–50 GiB retained-lab target. Record actual before/after allocation, including
both totals, and report the target as blocked until safe compaction and retention
have been measured. Host free space is reported separately and never substituted
for retained lab size.

The reviewed Windows 10 cleanup removed 14.781 GiB of unique allocated storage.
Before adding the shared recovery checkpoint, whole-tree allocation measured
75.239 GiB and environments 62.058 GiB. The 40–50 GiB target remains blocked.

Run the disposable safety checks with:

```sh
python3 -m unittest discover -s scripts/release -p test_lab.py -v
```
