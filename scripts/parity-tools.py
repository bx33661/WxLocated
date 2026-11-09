#!/usr/bin/env python3
"""Parity gate for `doctor`, `watch` and `overlay`.

- doctor: whole report, the executable path masked (the two binaries differ).
- watch: an app with nothing missing (started + complete), an app with
  missing subpackages and a 1 s timeout (started + timeout, exit 75), an
  unknown app and a missing argument; event timestamps masked.
- overlay: the full lifecycle on one exported material. init (both sides
  from the same material), the same edit and one added file, import, list,
  an export that picks the layers up (semantic/, material.json, AGENT.md),
  trace from a semantic layer, remove. Layers are written to the real store
  under test names and removed at the end.

    python3 -I scripts/parity-tools.py --reference <参照版本的 wxlocated> --rust target/release/wxlocated
"""

import argparse
import difflib
import json
import os
import re
import shutil
import subprocess
import sys

TIME = re.compile(r'("(?:at|created_at|generated_at|scanned_at)" ?: ?)"[^"]*"')
DURATION = re.compile(r'("(?:duration_seconds|durationMilliseconds|generatedAt|id)" : )("[^"]*"|[0-9.e+-]+)')


def run(binary, args, replacements=()):
    result = subprocess.run([binary, *args], capture_output=True, text=True)
    text = TIME.sub(r'\1"<t>"', result.stdout)
    for old, new in replacements:
        text = text.replace(old, new)
    return result.returncode, text


def report(label, swift, rust, failures):
    if swift == rust:
        return True
    diff = list(difflib.unified_diff(swift[1].splitlines(), rust[1].splitlines(), "swift", "rust", lineterm="", n=1))[2:12]
    failures.append((label, swift[0], rust[0], diff))
    return False


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference", "--swift", dest="swift", required=True, help="reference build: an earlier wxlocated, or the retired Swift app")
    parser.add_argument("--rust", required=True)
    parser.add_argument("--out", default=os.path.expanduser("~/Library/Caches/WxLocated-benchmark/parity-tools"))
    args = parser.parse_args()
    shutil.rmtree(args.out, ignore_errors=True)
    os.makedirs(args.out)
    failures, checks = [], 0
    listing = json.loads(subprocess.run([args.swift, "list", "--compact"], capture_output=True, text=True, check=True).stdout)
    apps = listing["miniapps"]

    # doctor
    masks = ((args.swift, "<exe>"), (os.path.abspath(args.rust), "<exe>"))
    checks += 1
    report("doctor", run(args.swift, ["doctor"], masks), run(args.rust, ["doctor"], masks), failures)

    # watch
    complete = next(a for a in apps if not a["missing_subpackages"])["app_id"]
    partial = next((a for a in apps if a["missing_subpackages"]), None)
    cases = [["watch", complete], ["watch"], ["watch", "wx0000000000000000"]]
    if partial:
        cases.append(["watch", partial["app_id"], "--timeout", "1", "--interval", "0.5"])
    for case in cases:
        checks += 1
        report(" ".join(case), run(args.swift, case), run(args.rust, case), failures)

    # overlay lifecycle
    app = next(a for a in apps if not a["missing_subpackages"])
    app_id = app["app_id"]
    material = os.path.join(args.out, "material")
    subprocess.run([args.swift, "export", app_id, "--out", material], capture_output=True, check=True)
    material_dir = os.path.join(material, os.listdir(material)[0])
    work = {side: os.path.join(args.out, f"work-{side}") for side in ("swift", "rust")}
    binaries = {"swift": args.swift, "rust": args.rust}
    results = {}
    for side in ("swift", "rust"):
        results[side] = run(binaries[side], ["overlay", "init", material_dir, work[side], "--name", f"parity-{side}", "--agent", "claude", "--model", "test"], ((work[side], "<work>"),))
    checks += 1
    report("overlay init", results["swift"], results["rust"], failures)
    manifests = {}
    for side in ("swift", "rust"):
        with open(os.path.join(work[side], "overlay.json")) as handle:
            manifests[side] = TIME.sub(r'\1"<t>"', handle.read()).replace(f"parity-{side}", "parity-<side>")
    checks += 1
    report("overlay.json", (0, manifests["swift"]), (0, manifests["rust"]), failures)

    # The same edit on both sides: change one rebuilt file, add a new one.
    manifest = json.loads(open(os.path.join(work["swift"], "overlay.json")).read())
    edited = next(f["path"] for f in manifest["files"] if f["path"].endswith(".js"))
    for side in ("swift", "rust"):
        with open(os.path.join(work[side], edited), "a") as handle:
            handle.write("\n// agent: explained\n")
        os.makedirs(os.path.join(work[side], "notes"), exist_ok=True)
        with open(os.path.join(work[side], "notes", "summary.md"), "w") as handle:
            handle.write("# summary\n")
        path = os.path.join(work[side], "overlay.json")
        data = json.loads(open(path).read())
        data["files"].append({"path": "notes/summary.md", "note": "added"})
        with open(path, "w") as handle:
            json.dump(data, handle, ensure_ascii=False, indent=2)
    for side in ("swift", "rust"):
        results[side] = run(binaries[side], ["overlay", "import", work[side]], ((f"parity-{side}", "parity-<side>"),))
    checks += 1
    report("overlay import", results["swift"], results["rust"], failures)

    checks += 1
    report("overlay list", run(args.swift, ["overlay", "list", app_id]), run(args.rust, ["overlay", "list", app_id]), failures)

    # Export picks both layers up; trace from a semantic layer.
    for side in ("swift", "rust"):
        out = os.path.join(args.out, f"export-{side}")
        subprocess.run([binaries[side], "export", app_id, "--out", out], capture_output=True, check=True)
    folder = os.listdir(os.path.join(args.out, "export-swift"))[0]
    for relative in ("material.json", "AGENT.md", "semantic/parity-swift/overlay.json", "semantic/parity-rust/" + edited, "semantic/parity-swift/notes/summary.md"):
        sides = []
        for side in ("swift", "rust"):
            path = os.path.join(args.out, f"export-{side}", folder, relative)
            text = open(path).read() if os.path.exists(path) else "<missing>"
            text = re.sub(r"于 \S+ 从本机微信缓存导出", "于 <t> 从本机微信缓存导出", DURATION.sub(r'\1"<v>"', TIME.sub(r'\1"<t>"', text)))
            sides.append((0, text))
        checks += 1
        report(f"export {relative}", sides[0], sides[1], failures)
    lines = open(os.path.join(work["swift"], edited)).read().count("\n")
    for side_layer in ("parity-swift", "parity-rust"):
        for line in (1, lines + 1):
            case = ["trace", os.path.join(args.out, "export-swift", folder), f"semantic/{side_layer}/{edited}:{line}"]
            checks += 1
            report(" ".join(case[2:]), run(args.swift, case), run(args.rust, case), failures)

    for side in ("swift", "rust"):
        results[side] = run(binaries[side], ["overlay", "remove", app_id, f"parity-{side}"], ((f"parity-{side}", "parity-<side>"),))
    checks += 1
    report("overlay remove", results["swift"], results["rust"], failures)
    checks += 1
    report("overlay remove (gone)", run(args.swift, ["overlay", "remove", app_id, "parity-swift"]), run(args.rust, ["overlay", "remove", app_id, "parity-swift"]), failures)

    print(f"检查 {checks} 项 · 一致 {checks - len(failures)}/{checks}")
    for label, swift_code, rust_code, diff in failures:
        print(f"  ✗ {label}  exit {swift_code}/{rust_code}")
        for line in diff:
            print(f"      {line[:200]}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
