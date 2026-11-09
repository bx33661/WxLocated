#!/usr/bin/env python3
"""Stage-1 parity gate for the Rust port: wxapkg unpacking.

Finds every .wxapkg under the cache roots the Swift CLI can read (main
packages, subpackages, plugins, older versions), dumps each with both
implementations (`__dump-entries`) and compares, per package: format, failure
code, notes, entry order (Swift sorts with localizedStandardCompare), and per
entry offset / size / SHA-256 / failure.

Output names local packages, so keep it out of the repo.

    python3 -I scripts/parity-archive.py --reference <参照版本的 wxlocated> --rust target/release/wxlocated
"""

import argparse
import glob
import json
import os
import subprocess
import sys

BATCH = 40


def dump(binary, paths):
    packages = []
    for start in range(0, len(paths), BATCH):
        chunk = paths[start:start + BATCH]
        run = subprocess.run([binary, "__dump-entries", "--compact", *chunk], capture_output=True, text=True)
        if run.returncode != 0:
            sys.exit(f"{binary} failed ({run.returncode}): {run.stderr[-2000:]}")
        packages.extend(json.loads(run.stdout)["packages"])
    return {package["path"]: package for package in packages}


def entry_key(entry):
    return (entry["offset"], entry["size"], entry.get("sha256"), entry.get("failure"))


def compare(swift, rust):
    """Returns a list of human-readable differences for one package."""
    diffs = []
    for field in ("format", "failure"):
        if swift.get(field) != rust.get(field):
            diffs.append(f"{field}: swift={swift.get(field)!r} rust={rust.get(field)!r}")
    if swift["notes"] != rust["notes"]:
        diffs.append(f"notes: swift={swift['notes']!r} rust={rust['notes']!r}")
    swift_entries = {entry["path"]: entry for entry in swift["entries"]}
    rust_entries = {entry["path"]: entry for entry in rust["entries"]}
    for path in sorted(swift_entries.keys() - rust_entries.keys()):
        diffs.append(f"only in swift: {path}")
    for path in sorted(rust_entries.keys() - swift_entries.keys()):
        diffs.append(f"only in rust: {path}")
    for path in sorted(swift_entries.keys() & rust_entries.keys()):
        if entry_key(swift_entries[path]) != entry_key(rust_entries[path]):
            diffs.append(f"entry {path}: swift={entry_key(swift_entries[path])} rust={entry_key(rust_entries[path])}")
    if not diffs and list(swift_entries) != list(rust_entries):
        first = next(i for i, (a, b) in enumerate(zip(swift_entries, rust_entries)) if a != b)
        diffs.append(f"order at {first}: swift={list(swift_entries)[first]} rust={list(rust_entries)[first]}")
    return diffs


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference", "--swift", dest="swift", required=True, help="reference build: an earlier wxlocated, or the retired Swift app")
    parser.add_argument("--rust", required=True, help="Rust wxlocated executable")
    parser.add_argument("--show", type=int, default=20, help="differences to print")
    args = parser.parse_args()

    listing = json.loads(subprocess.run([args.swift, "list", "--compact"], capture_output=True, text=True, check=True).stdout)
    paths = set()
    for root in listing["readable_roots"]:
        paths.update(glob.glob(os.path.join(root, "**", "*.wxapkg"), recursive=True))
    for app in listing["miniapps"]:
        for name in [app["main_package"], *app["subpackage_files"]]:
            paths.add(os.path.join(app["cache_directory"], name))
    paths = sorted(paths)

    swift = dump(args.swift, paths)
    rust = dump(args.rust, paths)

    mismatched = {}
    entries = 0
    for path in paths:
        diffs = compare(swift[path], rust[path])
        if diffs:
            mismatched[path] = diffs
        entries += len(swift[path]["entries"])

    failures = sum(1 for path in paths if swift[path].get("failure"))
    notes = sum(len(swift[path]["notes"]) for path in paths)
    print(f"包 {len(paths)} · 条目 {entries} · Swift 打开失败 {failures} · 备注 {notes}")
    print(f"一致 {len(paths) - len(mismatched)}/{len(paths)}(格式、失败、备注、条目顺序与每条偏移/大小/SHA-256)")
    shown = 0
    for path, diffs in mismatched.items():
        for diff in diffs:
            if shown >= args.show:
                break
            print(f"  {os.path.basename(os.path.dirname(path))}/{os.path.basename(path)}: {diff}")
            shown += 1
    return 1 if mismatched else 0


if __name__ == "__main__":
    sys.exit(main())
