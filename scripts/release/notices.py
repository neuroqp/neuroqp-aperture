#!/usr/bin/env python3
"""Collect installed Rust/JavaScript license notices and native bundle notices."""

import argparse
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def license_text(directory):
    files = sorted(p for p in directory.iterdir() if p.is_file() and p.name.lower().startswith(("license", "licence", "copying", "notice")))
    return "\n".join(f"{p.name}\n{p.read_text(errors='replace')}" for p in files)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("platform", choices=("linux", "windows"))
    parser.add_argument("native_notices", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    cargo = json.loads(subprocess.check_output(["cargo", "metadata", "--manifest-path", str(ROOT / "src-tauri/Cargo.toml"), "--locked", "--format-version", "1"], text=True))
    sections = ["Aperture third-party notices\nIncludes build dependencies and bundled runtime dependencies.\n"]
    for package in sorted(cargo["packages"], key=lambda p: (p["name"], p["version"])):
        if package["name"] == "aperture":
            continue
        directory = Path(package["manifest_path"]).parent
        text = license_text(directory)
        if package.get("license_file"):
            text += "\n" + (directory / package["license_file"]).read_text(errors="replace")
        sections.append(f"Rust: {package['name']} {package['version']}\nLicense: {package.get('license')}\nSource: {package.get('repository') or package.get('source')}\n{text}")
    for directory in sorted((ROOT / "node_modules/.pnpm").glob("*/node_modules/*")):
        candidates = [directory] if not directory.name.startswith("@") else sorted(directory.iterdir())
        for package_dir in candidates:
            metadata = package_dir / "package.json"
            if metadata.is_file() and not package_dir.is_symlink():
                package = json.loads(metadata.read_text())
                sections.append(f"JavaScript: {package.get('name')} {package.get('version')}\nLicense: {package.get('license')}\n{license_text(package_dir)}")
    sections.append(args.native_notices.read_text(errors="replace"))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text("\n\n".join(sections), encoding="utf-8")


if __name__ == "__main__":
    main()
