#!/usr/bin/env python3
"""Parity gate for the Rust port's decompilation stages.

For every cached app (main + subpackages as one archive, like `export`) and
every cached plugin package, runs the Swift oracle's `__dump-decompilation`
and the Rust one on the same input, then compares the artifacts of the given
kinds (path, confidence, content, kept entry, sources, notes), the
diagnostics of the given stages (in order) and the archive notes.

Stages are ported one by one, so the gate covers only what is ported:

    python3 -I scripts/parity-decompilation.py --reference <参照版本的 wxlocated> --rust target/release/wxlocated \\
        --kinds markup,wxs --stages indexing,wxs,markup

Use --reference-spec for an earlier Rust CLI, whose hidden dump command takes
an input specification rather than an AppID or --package.

Output names local apps; it is written outside the repo (--out).
"""

import argparse
import difflib
import json
import os
import subprocess
import sys
import time


def run_json(command):
    run = subprocess.run(command, capture_output=True, text=True)
    if run.returncode != 0:
        raise RuntimeError(f"{command[0]} {command[1]} failed ({run.returncode}): {run.stderr[-1500:]}")
    return json.loads(run.stdout)


def inputs(reference, out_dir, reference_spec):
    def command(legacy_args, specification):
        if not reference_spec:
            return [reference, "__dump-decompilation", *legacy_args, "--compact"]
        path = os.path.join(out_dir, "reference-spec.json")
        with open(path, "w") as handle:
            json.dump(specification, handle)
        return [reference, "__dump-decompilation", "--spec", path, "--compact"]

    listing = run_json([reference, "list", "--compact"])
    for app in listing["miniapps"]:
        specification = {
            "app_id": app["app_id"],
            "packages": [os.path.join(app["cache_directory"], name)
                         for name in [app["main_package"], *app["subpackage_files"]]],
            "declared_subpackages": app["declared_subpackages"],
            "standalone": False,
        }
        yield ("app", app["app_id"], command([app["app_id"]], specification))
        info = run_json([reference, "info", app["app_id"], "--compact"])
        for plugin in info["wxlocated"]["plugins"]:
            if not plugin.get("found") or not plugin.get("packages"):
                continue
            package = plugin["packages"][0]["source_path"]
            specification = {
                "app_id": plugin["provider"], "packages": [package],
                "declared_subpackages": 0, "standalone": True,
            }
            yield ("plugin", f"{app['app_id']}/{plugin['alias']}",
                   command(["--package", package, "--app-id", plugin["provider"]], specification))


def artifact_key(artifact):
    return (artifact["confidence"], artifact.get("archive_entry"), json.dumps(artifact["sources"], sort_keys=True),
            json.dumps(artifact["notes"], ensure_ascii=False))


def compare(swift, rust, kinds, stages, out_dir, label):
    diffs = []
    if swift["notes"] != rust["notes"]:
        diffs.append(f"notes: swift={swift['notes']} rust={rust['notes']}")
    pick = lambda result: {a["path"]: a for a in result["artifacts"] if a["kind"] in kinds}
    swift_artifacts, rust_artifacts = pick(swift), pick(rust)
    for path in sorted(swift_artifacts.keys() - rust_artifacts.keys()):
        diffs.append(f"only in swift: {path}")
    for path in sorted(rust_artifacts.keys() - swift_artifacts.keys()):
        diffs.append(f"only in rust: {path}")
    for path in sorted(swift_artifacts.keys() & rust_artifacts.keys()):
        a, b = swift_artifacts[path], rust_artifacts[path]
        if a.get("content") != b.get("content"):
            diffs.append(f"content {path}")
            base = os.path.join(out_dir, "diff", label.replace("/", "_"), path)
            os.makedirs(os.path.dirname(base), exist_ok=True)
            for side, artifact in (("swift", a), ("rust", b)):
                with open(f"{base}.{side}", "w") as handle:
                    handle.write(artifact.get("content") or "")
        if artifact_key(a) != artifact_key(b):
            diffs.append(f"meta {path}: swift={artifact_key(a)} rust={artifact_key(b)}")
    if list(swift_artifacts) != list(rust_artifacts) and not any(d.startswith("only in") for d in diffs):
        first = next(i for i, (x, y) in enumerate(zip(swift_artifacts, rust_artifacts)) if x != y)
        diffs.append(f"artifact order at {first}: swift={list(swift_artifacts)[first]} rust={list(rust_artifacts)[first]}")
    pick_diagnostics = lambda result: [(d["severity"], d["stage"], d["message"], json.dumps(d.get("source"), sort_keys=True))
                                       for d in result["diagnostics"] if d["stage"] in stages]
    swift_diagnostics, rust_diagnostics = pick_diagnostics(swift), pick_diagnostics(rust)
    if swift_diagnostics != rust_diagnostics:
        matcher = difflib.SequenceMatcher(a=swift_diagnostics, b=rust_diagnostics, autojunk=False)
        for tag, i1, i2, j1, j2 in matcher.get_opcodes():
            if tag != "equal":
                diffs.append(f"diagnostics {tag}: swift={swift_diagnostics[i1:i2][:3]} rust={rust_diagnostics[j1:j2][:3]}")
    return diffs, len(swift_artifacts)


def first_content_diff(out_dir, label, path, lines=30):
    base = os.path.join(out_dir, "diff", label.replace("/", "_"), path)
    with open(f"{base}.swift") as a, open(f"{base}.rust") as b:
        diff = list(difflib.unified_diff(a.read().splitlines(), b.read().splitlines(), "swift", "rust", lineterm="", n=2))
    return "\n".join(diff[:lines])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference", "--swift", dest="swift", required=True, help="reference build: an earlier wxlocated, or the retired Swift app")
    parser.add_argument("--reference-spec", action="store_true", help="reference dump command uses --spec (earlier Rust CLI)")
    parser.add_argument("--rust", required=True)
    parser.add_argument("--kinds", default="markup,wxs")
    parser.add_argument("--stages", default="indexing,wxs,markup")
    parser.add_argument("--out", default=os.path.expanduser("~/Library/Caches/WxLocated-benchmark/parity"))
    parser.add_argument("--only", help="limit to labels containing this text")
    parser.add_argument("--show", type=int, default=12)
    args = parser.parse_args()
    kinds, stages = set(args.kinds.split(",")), set(args.stages.split(","))
    os.makedirs(args.out, exist_ok=True)

    total = matched = artifacts = 0
    failures = []
    swift_seconds = rust_seconds = 0.0
    for kind, label, command in inputs(args.swift, args.out, args.reference_spec):
        if args.only and args.only not in label:
            continue
        total += 1
        started = time.time()
        swift = run_json(command)
        swift_seconds += time.time() - started
        spec = os.path.join(args.out, "spec.json")
        with open(spec, "w") as handle:
            json.dump(swift["input"], handle)
        started = time.time()
        try:
            rust = run_json([args.rust, "__dump-decompilation", "--spec", spec, "--compact"])
        except RuntimeError as error:
            failures.append((label, [f"rust failed: {error}"]))
            continue
        rust_seconds += time.time() - started
        diffs, count = compare(swift, rust, kinds, stages, args.out, label)
        artifacts += count
        if diffs:
            failures.append((label, diffs))
        else:
            matched += 1

    print(f"输入 {total}(应用 + 插件)· 比较的产物 {artifacts} · 种类 {sorted(kinds)} · 诊断阶段 {sorted(stages)}")
    print(f"一致 {matched}/{total} · 用时 Swift {swift_seconds:.1f}s / Rust {rust_seconds:.1f}s")
    shown = 0
    for label, diffs in failures:
        print(f"  ✗ {label}: {len(diffs)} 处")
        for diff in diffs[:3]:
            print(f"      {diff[:300]}")
        if shown < args.show:
            content = next((d for d in diffs if d.startswith("content ")), None)
            if content:
                print(first_content_diff(args.out, label, content[len("content "):]))
            shown += 1
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
