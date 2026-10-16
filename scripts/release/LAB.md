# QA lab maintenance

Prepare desktops before package smoke. Reuse the existing Windows 11 disk. The
Debian 12 and Ubuntu 24 labs retain local live ISOs and bootstrap inputs rather
than persistent installed bases; restore their sessions from those inputs and
check the actual desktop before smoke. Ubuntu needs a native Wayland session. Arch/Omarchy
and Fedora are periodic compatibility checks and required for relevant runtime or
desktop changes. Lab maintenance does not establish application acceptance.

The host lab is `artifacts/first-release` in this checkout. Use `lab.py`
from this Linux host. It downloads nothing and saves no guest RAM. Every mutation
requires owned QEMU and swtpm processes to have stopped. Launchers hold
`environments/bounded-lab/operation.lock` for the whole VM lifetime.

## Storage and data safety

Keep reusable sparse read-only bases and small disposable qcow2 overlays. Retain
the current and previous relevant packages, source inputs and compact reports.
Keep disks, TPM/UEFI state and private credential files together. Never print
credentials. Do not keep a full lab copy for every retry.

Record exact owned paths and identities before cleanup. Never delete keys,
user-owned disks, real microscopy datasets, previous stable packages or source
archives. Duplicate names do not prove duplicate contents. Count hardlinks once;
Btrfs reflinks can share extents, so allocation totals differ from host free space.
The existing cleanup helper intentionally supports only its exact allowlist.

```sh
python3 scripts/release/lab.py --output artifacts/lab-before.json inventory --btrfs
python3 scripts/release/lab.py --output artifacts/lab-cleanup-plan.json plan-cleanup
# Inspect the exact path and identity manifest before applying it.
python3 scripts/release/lab.py --output artifacts/lab-cleanup-result.json cleanup artifacts/lab-cleanup-plan.json
python3 scripts/release/lab.py --output artifacts/lab-after.json inventory --btrfs
```

Before removing installation media, prove a disk-only boot with the actual disk
identity. Before freezing or compacting a Windows base, observe full cold guest
shutdown with `shutdown /s /t 0`, QMP SHUTDOWN, exit 0 and no preceding SUSPEND_DISK.
A stopped or killed host process does not prove clean guest shutdown. An overlay's
clean shutdown does not certify its backing disk.

```sh
python3 scripts/release/lab.py --output artifacts/lab-compacted.json compact RECOVERY_JSON
python3 scripts/release/lab.py --output artifacts/lab-frozen.json freeze-base artifacts/lab-compacted.json
python3 scripts/release/lab.py --output artifacts/lab-run.json overlay RUN_ID
```

Compaction uses an offline temporary copy and `qemu-img compare` before replacing
an unchanged original. It needs the current allocated size plus 4 GiB free space.
Never compact an image backing an overlay. On timeout, stop the owned converter
process group, retain the original and diagnostic output, and inspect temporary
files before exact-path cleanup. Preserve rollback disk/TPM/UEFI state until a
verified replacement is durable. A rollback reflink can grow as its source changes.
A helper's run-size budget is not an enforced storage quota; measure actual use.

## Native session and transport

Keep observers, fixed TIFF, helper hashes, strict host-key checks and capture
commands prepared and rehearsed. Observe the actual graphical session and user.
Use only the owned VM/session for testing. Do not alter host desktop preferences
to keep a lab awake. Stop owned observers and idle guards after use, then shut
down guests normally.

For the existing Windows WinRM forwarding, probe without credentials:

```sh
curl --noproxy '*' -i --connect-timeout 2 --max-time 3 http://127.0.0.1:15985/wsman
```

HTTP 405 proves an endpoint response, not authenticated observer readiness. A
timeout does not establish bad credentials. Check the guest's WinRM service,
listener and firewall rule before changing authentication. Preserve existing
rules. Any isolated-VM allowance must remain restricted to the QEMU host address,
`10.0.2.2`; never disable the firewall. Use the existing private credential file
without emitting its contents.

After a repair, rehearse launch, fixed-TIFF visible rendering and normal close
with the exact prepared adapter. Then repeat the affected package check. Preserve
failed reports; successful unchanged source and package hashes remain usable.
Run `python3 -m unittest discover -s scripts/release -p test_lab.py -v` after
changing the lab helper. Record actual storage measurements in external evidence,
not dated claims in this procedure.
