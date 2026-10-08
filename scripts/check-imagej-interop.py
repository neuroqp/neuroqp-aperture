#!/usr/bin/env python3
"""Check TIFF annotation interoperability against an installed ImageJ ij.jar.

Usage: python3 scripts/check-imagej-interop.py /path/to/ij.jar
The jar argument can be omitted when IMAGEJ_JAR is set.
Requires a JDK and Cargo with the project's serde dependency cached. Set
JAVA_HOME or pass --java-home when the JDK is not on PATH. All synthetic
images and builds live in a temporary directory; no real images are read.
"""

import argparse
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("jar", type=Path, nargs="?", default=os.environ.get("IMAGEJ_JAR"))
    parser.add_argument("--java-home", type=Path, default=os.environ.get("JAVA_HOME"))
    args = parser.parse_args()
    if args.jar is None:
        parser.error("pass /path/to/ij.jar or set IMAGEJ_JAR")
    jar = args.jar.resolve(strict=True)
    scripts = Path(__file__).resolve().parent
    backend = scripts.parent / "src-tauri" / "src"
    java = str(args.java_home / "bin" / "java") if args.java_home else "java"
    javac = str(args.java_home / "bin" / "javac") if args.java_home else "javac"

    def run(*command):
        subprocess.run(command, check=True)

    with tempfile.TemporaryDirectory(prefix="aperture-imagej-") as directory:
        work = Path(directory)
        run(
            javac, "-cp", str(jar), "-d", directory,
            str(scripts / "check-imagej-interop.java"),
        )
        classpath = os.pathsep.join([directory, str(jar)])
        def imagej(mode):
            run(java, "-Djava.awt.headless=true", "-cp", classpath,
                "ImageJInterop", mode, directory)

        imagej("generate")
        sources = {path.name: path.read_bytes() for path in work.glob("*.tif")}
        (work / "Cargo.toml").write_text(
            '[package]\nname="aperture-imagej-check"\nversion="0.0.0"\nedition="2021"\n'
            '[dependencies]\nserde={version="1",features=["derive"]}\n'
        )
        (work / "src").mkdir()
        shutil.copyfile(scripts / "check-imagej-interop.rs", work / "src" / "main.rs")
        for module in ["annotation_io.rs", "roi.rs", "tiff_annotations.rs"]:
            shutil.copyfile(backend / module, work / "src" / module)
        def rust(mode):
            run("cargo", "run", "--quiet", "--offline", "--manifest-path",
                str(work / "Cargo.toml"), "--", mode, directory)

        rust("edit")
        for name, original in sources.items():
            assert (work / name).read_bytes() == original, f"Source modified: {name}"
            saved = (work / ("saved-" + name)).read_bytes()
            # Classic TIFF's first-IFD pointer is the only original byte changed.
            assert (
                saved[:4] == original[:4]
                and saved[8:len(original)] == original[8:]
            ), name
        imagej("verify")
        rust("reopen")
    print("PASS: ImageJ TIFF import, editable ROI/overlays, styles, counters, "
          "subpixels, metadata and exact pixel preservation")


if __name__ == "__main__":
    main()
