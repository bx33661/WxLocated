#!/usr/bin/env python3
"""Parity gate for `wxlocated export`: exports every cached app with the Swift
CLI and the Rust CLI into separate folders and compares the result document,
the file tree and every file byte for byte.

Masked, because they change on every run: timestamps and durations in
material.json, generatedAt / durationMilliseconds / diagnostic UUIDs in
WxLocated-decompilation.json, and the export-time line of AGENT.md.

Also checks option variants on the first app (--no-raw, --no-source,
--redact, --reuse, and the error when the destination exists).

    python3 -I scripts/parity-export.py --reference <参照版本的 wxlocated> --rust target/release/wxlocated

Output names local apps; it is written outside the repo (--out).
"""

import argparse
import difflib
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys

MATERIAL_VOLATILE = re.compile(r'("(?:generated_at|scanned_at|duration_seconds)" : )("[^"]*"|[0-9.e+-]+)')
MANIFEST_VOLATILE = re.compile(r'("(?:generatedAt|durationMilliseconds|id)" : )("[^"]*"|[0-9]+)')
README_VOLATILE = re.compile(r"于 \S+ 从本机微信缓存导出")


def masked(path):
    name = os.path.basename(path)
    with open(path, "rb") as handle:
        data = handle.read()
    if name == "material.json":
        return MATERIAL_VOLATILE.sub(r'\1"<v>"', data.decode()).encode()
    if name == "WxLocated-decompilation.json":
        return MANIFEST_VOLATILE.sub(r'\1"<v>"', data.decode()).encode()
    if name == "AGENT.md":
        return README_VOLATILE.sub("于 <v> 从本机微信缓存导出", data.decode()).encode()
    return data


def tree(root):
    files = {}
    for directory, _, names in os.walk(root):
        for name in names:
            path = os.path.join(directory, name)
            files[os.path.relpath(path, root)] = path
    return files


def run(binary, args, base):
    result = subprocess.run([binary, *args], capture_output=True, text=True)
    return result.returncode, result.stdout.replace(base, "<out>")


def compare_trees(swift_root, rust_root):
    a, b = tree(swift_root), tree(rust_root)
    diffs = [f"only in swift: {p}" for p in sorted(a.keys() - b.keys())]
    diffs += [f"only in rust: {p}" for p in sorted(b.keys() - a.keys())]
    for path in sorted(a.keys() & b.keys()):
        left, right = masked(a[path]), masked(b[path])
        if left != right:
            detail = ""
            if path.endswith((".json", ".md")):
                lines = difflib.unified_diff(left.decode(errors="replace").splitlines(), right.decode(errors="replace").splitlines(), lineterm="", n=0)
                detail = " | ".join(list(lines)[2:6])
            diffs.append(f"differs: {path} {hashlib.sha256(left).hexdigest()[:8]}≠{hashlib.sha256(right).hexdigest()[:8]} {detail[:300]}")
    return diffs, len(a)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference", "--swift", dest="swift", required=True, help="reference build: an earlier wxlocated, or the retired Swift app")
    parser.add_argument("--rust", required=True)
    parser.add_argument("--out", default=os.path.expanduser("~/Library/Caches/WxLocated-benchmark/parity-export"))
    parser.add_argument("--only", help="limit to one AppID")
    args = parser.parse_args()
    shutil.rmtree(args.out, ignore_errors=True)
    os.makedirs(args.out)

    listing = json.loads(subprocess.run([args.swift, "list", "--compact"], capture_output=True, text=True, check=True).stdout)
    app_ids = [app["app_id"] for app in listing["miniapps"] if not args.only or app["app_id"] == args.only]

    failures, files = [], 0
    for app_id in app_ids:
        swift_out, rust_out = os.path.join(args.out, "swift"), os.path.join(args.out, "rust")
        swift_code, swift_doc = run(args.swift, ["export", app_id, "--out", swift_out], swift_out)
        rust_code, rust_doc = run(args.rust, ["export", app_id, "--out", rust_out], rust_out)
        diffs = []
        if (swift_code, swift_doc) != (rust_code, rust_doc):
            diffs.append(f"result document: exit {swift_code}/{rust_code}\n{swift_doc[:400]}\n---\n{rust_doc[:400]}")
        folder = json.loads(swift_doc)["root"].replace("<out>/", "") if swift_code == 0 else None
        if folder:
            tree_diffs, count = compare_trees(os.path.join(swift_out, folder), os.path.join(rust_out, folder))
            diffs += tree_diffs
            files += count
        if diffs:
            failures.append((app_id, diffs))

    # Option variants on the first app.
    variants = [["--no-raw"], ["--no-source"], ["--redact"]]
    first = app_ids[0]
    for variant in variants:
        base = os.path.join(args.out, "variant")
        shutil.rmtree(base, ignore_errors=True)
        swift_out, rust_out = os.path.join(base, "swift"), os.path.join(base, "rust")
        swift_code, swift_doc = run(args.swift, ["export", first, "--out", swift_out, *variant], swift_out)
        rust_code, rust_doc = run(args.rust, ["export", first, "--out", rust_out, *variant], rust_out)
        diffs = [] if (swift_code, swift_doc) == (rust_code, rust_doc) else [f"result document {variant}: {swift_doc[:300]} --- {rust_doc[:300]}"]
        folder = json.loads(swift_doc)["root"].replace("<out>/", "")
        tree_diffs, _ = compare_trees(os.path.join(swift_out, folder), os.path.join(rust_out, folder))
        diffs += tree_diffs
        # Second run: --reuse returns the existing material; without flags it refuses.
        for extra in (["--reuse", *variant], []):
            swift_again = run(args.swift, ["export", first, "--out", swift_out, *extra], swift_out)
            rust_again = run(args.rust, ["export", first, "--out", rust_out, *extra], rust_out)
            if swift_again != rust_again:
                diffs.append(f"rerun {extra}: {swift_again} --- {rust_again}")
        if diffs:
            failures.append((f"{first} {' '.join(variant)}", diffs))

    print(f"应用 {len(app_ids)} · 比较文件 {files} · 选项变体 {len(variants)}(含 --reuse 与目标已存在)")
    print(f"一致 {len(app_ids) + len(variants) - len(failures)}/{len(app_ids) + len(variants)}")
    for label, diffs in failures[:8]:
        print(f"  ✗ {label}: {len(diffs)} 处")
        for diff in diffs[:4]:
            print(f"      {diff[:500]}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
