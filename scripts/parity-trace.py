#!/usr/bin/env python3
"""Parity gate for `wxlocated trace`: runs the Swift and the Rust CLI on the
same exported materials and compares output byte for byte and exit codes.

Uses the materials written by scripts/parity-export.py (its Swift side), and
samples per app: passthrough files (first, middle and out-of-range lines),
generated files (first, middle and last lines, short lines such as `}`),
plugin files, the `source/` prefix, no line, `--context`, unknown files, and
a directory that is not a material.

    python3 -I scripts/parity-trace.py --reference <参照版本的 wxlocated> --rust target/release/wxlocated
"""

import argparse
import difflib
import glob
import json
import os
import random
import subprocess
import sys


def run(binary, args):
    result = subprocess.run([binary, "trace", *args], capture_output=True, text=True)
    return result.returncode, result.stdout


def cases_for(material):
    cases = [[material, "does/not/exist.js:3"], [material + "/raw", "app.js:1"], [material]]
    manifest_path = os.path.join(material, "source", "WxLocated-decompilation.json")
    if not os.path.exists(manifest_path):
        return cases
    with open(manifest_path) as handle:
        artifacts = json.load(handle)["artifacts"]
    rng = random.Random(os.path.basename(material))
    passthrough = [a for a in artifacts if not a["generated"] and a["path"].endswith((".js", ".json", ".wxml", ".wxss", ".html"))]
    generated = [a for a in artifacts if a["generated"] and a["sources"] and a["sources"][0].get("startOffset") is not None]
    whole = [a for a in artifacts if a["generated"] and a["sources"] and a["sources"][0].get("startOffset") is None]
    for artifact in rng.sample(passthrough, min(3, len(passthrough))):
        lines = open(os.path.join(material, "source", artifact["path"]), errors="replace").read().count("\n") + 1
        cases += [[material, f"{artifact['path']}:1"], [material, f"source/{artifact['path']}:{max(1, lines // 2)}"], [material, f"{artifact['path']}:{lines + 5}"]]
    for artifact in rng.sample(generated, min(6, len(generated))) + rng.sample(whole, min(2, len(whole))):
        text = open(os.path.join(material, "source", artifact["path"]), errors="replace").read().split("\n")
        picks = {1, max(1, len(text) // 2), len(text)}
        short = [i + 1 for i, line in enumerate(text) if line.strip() in ("}", "})", "};", "</view>")]
        picks.update(short[:2])
        for line in sorted(picks):
            cases.append([material, f"{artifact['path']}:{line}"])
        cases.append([material, artifact["path"]])
        cases.append([material, f"{artifact['path']}:{max(1, len(text) // 3)}", "--context", "30"])
    for plugin_source in glob.glob(os.path.join(material, "plugins", "*", "source", "WxLocated-decompilation.json"))[:1]:
        plugin_dir = os.path.dirname(plugin_source)
        with open(plugin_source) as handle:
            plugin_artifacts = [a for a in json.load(handle)["artifacts"] if a["generated"]][:2]
        relative = os.path.relpath(plugin_dir, material)
        for artifact in plugin_artifacts:
            cases.append([material, f"{relative}/{artifact['path']}:2"])
    return cases


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference", "--swift", dest="swift", required=True, help="reference build: an earlier wxlocated, or the retired Swift app")
    parser.add_argument("--rust", required=True)
    parser.add_argument("--materials", default=os.path.expanduser("~/Library/Caches/WxLocated-benchmark/parity-export/swift"))
    args = parser.parse_args()
    materials = sorted(glob.glob(os.path.join(args.materials, "wx*")))
    if not materials:
        sys.exit("no materials: run scripts/parity-export.py first")
    cases = [["missing"]]
    for material in materials:
        cases += cases_for(material)

    mismatched, methods = [], {}
    for case in cases:
        swift, rust = run(args.swift, case), run(args.rust, case)
        if swift != rust:
            mismatched.append((case, swift, rust))
        try:
            method = json.loads(swift[1]).get("match", {}).get("method") or json.loads(swift[1])["error"]["code"]
            methods[method] = methods.get(method, 0) + 1
        except (ValueError, KeyError):
            pass
    print(f"trace 调用 {len(cases)} · 一致 {len(cases) - len(mismatched)}/{len(cases)} · 结果分布 {dict(sorted(methods.items()))}")
    for case, swift, rust in mismatched[:6]:
        print(f"  ✗ {' '.join(os.path.basename(c) if c.startswith('/') else c for c in case)}  exit {swift[0]}/{rust[0]}")
        for line in list(difflib.unified_diff(swift[1].splitlines(), rust[1].splitlines(), "swift", "rust", lineterm="", n=1))[2:12]:
            print(f"      {line[:200]}")
    return 1 if mismatched else 0


if __name__ == "__main__":
    sys.exit(main())
