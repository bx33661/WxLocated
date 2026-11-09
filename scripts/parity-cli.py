#!/usr/bin/env python3
"""Parity gate for the Rust CLI's JSON documents: runs the same command with
the Swift CLI and the Rust CLI and compares the output byte for byte, after
masking fields that change on every run (timestamps, durations).

    python3 -I scripts/parity-cli.py --reference <参照版本的 wxlocated> --rust target/release/wxlocated

Covers `list`, `info <appid>` for every cached app, and the error documents
for an unknown app and a missing argument. Output names local apps; it is
written outside the repo (--out).
"""

import argparse
import difflib
import json
import os
import re
import subprocess
import sys

VOLATILE = re.compile(r'("(?:generated_at|scanned_at|duration_seconds)" ?: ?)("[^"]*"|[0-9.e+-]+)')


def run(binary, args):
    result = subprocess.run([binary, *args], capture_output=True, text=True)
    return result.returncode, VOLATILE.sub(r'\1"<volatile>"', result.stdout)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference", "--swift", dest="swift", required=True, help="reference build: an earlier wxlocated, or the retired Swift app")
    parser.add_argument("--rust", required=True)
    parser.add_argument("--out", default=os.path.expanduser("~/Library/Caches/WxLocated-benchmark/parity-cli"))
    args = parser.parse_args()
    os.makedirs(args.out, exist_ok=True)

    _, listing = run(args.swift, ["list", "--compact"])
    app_ids = [app["app_id"] for app in json.loads(listing)["miniapps"]]
    cases = [["list"], ["list", "--compact"], ["info"], ["info", "wx0000000000000000"]]
    cases += [[alias, *options] for alias in ("help", "--help", "-h") for options in ([], ["--compact"])]
    cases += [["info", app_id] for app_id in app_ids]

    mismatched = []
    for case in cases:
        swift_code, swift_text = run(args.swift, case)
        rust_code, rust_text = run(args.rust, case)
        if (swift_code, swift_text) != (rust_code, rust_text):
            mismatched.append((case, swift_code, rust_code, swift_text, rust_text))

    print(f"命令 {len(cases)} · 一致 {len(cases) - len(mismatched)}/{len(cases)}(输出逐字节、退出码;时间戳与耗时已屏蔽)")
    for case, swift_code, rust_code, swift_text, rust_text in mismatched[:6]:
        print(f"  ✗ {' '.join(case)}  exit swift={swift_code} rust={rust_code}")
        diff = difflib.unified_diff(swift_text.splitlines(), rust_text.splitlines(), "swift", "rust", lineterm="", n=1)
        for line in list(diff)[2:14]:
            print(f"      {line}")
    return 1 if mismatched else 0


if __name__ == "__main__":
    sys.exit(main())
