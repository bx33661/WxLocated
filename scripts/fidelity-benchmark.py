#!/usr/bin/env python3
"""Reconstruction fidelity benchmark on the local WeChat cache.

Exports every cached mini program with `wxlocated export --no-raw`, then
measures how much of what the compiler registered was actually restored.

Ground truth comes from the compiled output itself: every
`__wxAppCode__["x.wxml"]` / `__wxAppCode__["x.wxss"]` registration in the
raw container files is a template or style that should come back as a file.

Results contain local app names and AppIDs; keep them out of the repository.

    python3 -I scripts/fidelity-benchmark.py --bin target/release/wxlocated --out <结果目录>
"""

import argparse
import ast
import json
import os
import re
import subprocess
import sys
import time

REGISTRATION = re.compile(rb"""__wxAppCode__\[\s*['"]([^'"]+\.(wxml|wxss))['"]\s*\]\s*=""")
PLUGIN_PREFIX = re.compile(r"^(plugin|plugin-private)://")


def plugin_paths(paths):
    """`plugin-private://<id>/x` → `__plugin__/<id>/x`, where WxLocated writes plugin files."""
    return {PLUGIN_PREFIX.sub("__plugin__/", path) for path in paths}


def normalize(path):
    path = path.replace("\\", "/")
    while path.startswith("./"):
        path = path[2:]
    return path.lstrip("/")


def registrations(source_root):
    """Template/style paths registered in the raw container copies."""
    found = {"wxml": set(), "wxss": set(), "plugin_wxml": set(), "plugin_wxss": set()}
    raw_root = os.path.join(source_root, "__wxlocated_raw__")
    for root, _, files in os.walk(raw_root):
        for name in files:
            if not name.endswith((".js", ".html")):
                continue
            data = open(os.path.join(root, name), "rb").read()
            for match in REGISTRATION.finditer(data):
                path = match.group(1).decode("utf-8", "replace")
                kind = match.group(2).decode()
                if PLUGIN_PREFIX.match(path):
                    found["plugin_" + kind].add(path)
                else:
                    found[kind].add(normalize(path))
    return found


RENDER_LAYER = ("page-frame.js", "app-wxss.js", "page-frame.html")
SEGMENT = re.compile(
    r"var x=(\[[^\]]*\])"
    r"|var (m\d+)=function\(e,s,r,gg\)\{"
    r"|e_\[x\[(\d+)\]\]=\{f:(m\d+)"
    r"""|d_\[x\[(\d+)\]\]\[["']([^"']+)["']\]=function\([^)]*\)\{"""
)
WRAPPER = re.compile(r"(\$gwx\w*)\s*=\s*function")
PLUGIN_TEMPLATE = re.compile(
    r"""__wxAppCode__\[['"](plugin(?:-private)?://[^'"]+\.wxml)['"]\]\s*=\s*\[?\s*(\$gwx\w+)\s*[,(]\s*['"]([^'"]+)['"]""")
ELEMENT_CALL = re.compile(r"(?<![A-Za-z0-9_$])_(?:n|m|mz)\(")
# glass-easel (componentFramework): templates live in batchAddCompiledTemplate
# return objects, keyed by component/page path; there is no __wxAppCode__ wxml
# registration for them, so they must be counted separately.
GE_TEMPLATE_OBJ = re.compile(r"batchAddCompiledTemplate\(function\(")
GE_KEY = re.compile(r"(?:^|[{,])\s*'([^']+?)':\s*\(\(\)=>\{")
OUTPUT_TAG = re.compile(r"<([A-Za-z_$][A-Za-z0-9_$:-]*)([^>]*)>")


# A real element creation is `E("tag"…`; `E(N)`/`E(N,…)` inside binding-map
# update closures is an end-marker, not an element. `S(…)` is a <slot>.
GE_ELEMENT = re.compile(r"""(?<![\w.$])(?:E\(['"]|S\()""")


def match_brace(text, open_index):
    """Index just past the `}` matching the `{` at open_index."""
    depth = 0
    for i in range(open_index, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return i
    return len(text)


def glass_easel_entries(source_root):
    """Each glass-easel template path → the element-creating `E(` count across
    all its H-key bodies (its root plus any named <template> it declares)."""
    counts = {}
    raw_root = os.path.join(source_root, "__wxlocated_raw__")
    render = {}  # keep the render-layer (non app-service) count when duplicated
    for root, _, files in os.walk(raw_root):
        for name in files:
            if not name.endswith(".js") or "batchAddCompiledTemplate" not in \
                    open(os.path.join(root, name), encoding="utf-8", errors="replace").read():
                continue
            text = open(os.path.join(root, name), encoding="utf-8", errors="replace").read()
            logic = "app-service" in name or "appservice" in name or name == "common.app.js"
            for key in GE_KEY.finditer(text):
                # GE_KEY ends at the IIFE's `=>{`; the body is from that brace
                # to its match, covering every H[...] render in the entry.
                brace = key.end() - 1
                end = match_brace(text, brace)
                path = normalize(key.group(1)) + ".wxml"
                count = len(GE_ELEMENT.findall(text[brace:end]))
                if logic and path in render:
                    continue
                counts[path] = count
                if not logic:
                    render[path] = count
    return counts


def glass_easel_templates(source_root):
    return set(glass_easel_entries(source_root))


def glass_easel_structure(source_root):
    """Restored glass-easel templates whose element count matches the compile."""
    expected = glass_easel_entries(source_root)
    exact, mismatched = 0, []
    for path, count in sorted(expected.items()):
        target = os.path.join(source_root, path)
        if not os.path.exists(target):
            continue
        got = restored_elements(open(target, encoding="utf-8", errors="replace").read())
        if got == count:
            exact += 1
        else:
            mismatched.append(f"{path}: {got}/{count}")
    return {"compared": exact + len(mismatched), "exact": exact, "mismatch_examples": mismatched[:6]}


def body_at(data, open_brace):
    depth = 0
    for index in range(open_brace, len(data)):
        char = data[index]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return data[open_brace:index]
    return data[open_brace:]


def compiled_elements(source_root):
    """Elements each template creates, counted from the render-layer factories."""
    counts = {}
    raw_root = os.path.join(source_root, "__wxlocated_raw__")
    for root, _, files in os.walk(raw_root):
        for name in files:
            if name not in RENDER_LAYER:
                continue
            data = open(os.path.join(root, name), encoding="utf-8", errors="replace").read()
            paths, bodies = [], {}
            wrappers = [(m.start(), m.group(1)) for m in WRAPPER.finditer(data)]
            plugin_owned = {(m.group(2), normalize(m.group(3))): PLUGIN_PREFIX.sub("__plugin__/", m.group(1))
                            for m in PLUGIN_TEMPLATE.finditer(data)}

            def output_path(raw, position):
                path = normalize(raw)
                wrapper = next((name for start, name in reversed(wrappers) if start < position), None)
                return plugin_owned.get((wrapper, path), path)

            for match in SEGMENT.finditer(data):
                if match.group(1):
                    try:
                        paths = ast.literal_eval(match.group(1))
                    except (ValueError, SyntaxError):
                        paths = []
                elif match.group(2):
                    bodies[match.group(2)] = body_at(data, match.end() - 1)
                elif match.group(3):
                    index, function = int(match.group(3)), match.group(4)
                    if index < len(paths) and function in bodies:
                        path = output_path(paths[index], match.start())
                        counts.setdefault(path, {"factory": 0, "templates": 0})
                        counts[path]["factory"] = len(ELEMENT_CALL.findall(bodies[function]))
                elif match.group(5):
                    index = int(match.group(5))
                    if index < len(paths):
                        path = output_path(paths[index], match.start())
                        counts.setdefault(path, {"factory": 0, "templates": 0})
                        counts[path]["templates"] += len(ELEMENT_CALL.findall(body_at(data, match.end() - 1)))
    return {path: value["factory"] + value["templates"] for path, value in counts.items()}


def restored_elements(text):
    text = re.sub(r"\{\{.*?\}\}", "", text, flags=re.S)
    count = 0
    for tag, attributes in OUTPUT_TAG.findall(text):
        if tag in ("import", "include", "wxs", "template"):
            continue
        if tag == "block" and "wx:" in attributes:
            continue
        count += 1
    return count


def structure(source_root, registered_wxml):
    expected = compiled_elements(source_root)
    exact, mismatched = 0, []
    for path in sorted(registered_wxml):
        target = os.path.join(source_root, path)
        if path not in expected or not os.path.exists(target):
            continue
        got = restored_elements(open(target, encoding="utf-8", errors="replace").read())
        if got == expected[path]:
            exact += 1
        else:
            mismatched.append(f"{path}: {got}/{expected[path]}")
    return {"compared": exact + len(mismatched), "exact": exact, "mismatch_examples": mismatched[:5]}


IMPORT = re.compile(r"""@import\s+["']([^"']+)["']""")


def broken_imports(source_root, registered_wxss):
    """@import targets in restored styles that do not exist in the material."""
    broken = []
    for path in sorted(registered_wxss):
        target = os.path.join(source_root, path)
        if not os.path.exists(target):
            continue
        for reference in IMPORT.findall(open(target, encoding="utf-8", errors="replace").read()):
            base = source_root if reference.startswith("/") else os.path.dirname(target)
            resolved = os.path.normpath(os.path.join(base, reference.lstrip("/")))
            if not os.path.exists(resolved):
                broken.append(f"{path} -> {reference}")
    return broken


def declared_pages(source_root):
    app_json = os.path.join(source_root, "app.json")
    if not os.path.exists(app_json):
        return []
    try:
        app = json.load(open(app_json))
    except ValueError:
        return []
    pages = list(app.get("pages", []))
    for package in app.get("subPackages", app.get("subpackages", [])) or []:
        root = package.get("root", "").strip("/")
        pages += [f"{root}/{page}" for page in package.get("pages", [])]
    return [normalize(page) for page in pages]


def measure(material_root, seconds):
    source = os.path.join(material_root, "source")
    manifest = json.load(open(os.path.join(source, "WxLocated-decompilation.json")))
    material = json.load(open(os.path.join(material_root, "material.json")))
    registered = registrations(source)

    def restored(paths):
        return sorted(path for path in paths if os.path.exists(os.path.join(source, path)))

    pages = declared_pages(source)
    page_files = {
        ext: sum(os.path.exists(os.path.join(source, page + ext)) for page in pages)
        for ext in (".js", ".json", ".wxml", ".wxss")
    }
    diagnostics = {}
    for item in manifest.get("diagnostics", []):
        key = re.sub(r"[：:].*$", "", item["message"])[:40]
        diagnostics[key] = diagnostics.get(key, 0) + 1
    confidence = {}
    for artifact in manifest["artifacts"]:
        confidence[artifact["confidence"]] = confidence.get(artifact["confidence"], 0) + 1

    return {
        "app_id": material["metadata"]["app_id"],
        "name": material["metadata"].get("app_name"),
        "completeness": material["completeness"],
        "seconds": round(seconds, 2),
        "wxml": {"registered": len(registered["wxml"]), "restored": len(restored(registered["wxml"])),
                 "missing_examples": sorted(set(registered["wxml"]) - set(restored(registered["wxml"])))[:5]},
        "wxml_structure": structure(source, registered["wxml"]),
        "glass_easel": {"registered": len(glass_easel_templates(source)),
                        "restored": len(restored(glass_easel_templates(source))),
                        "missing_examples": sorted(glass_easel_templates(source)
                                                   - set(restored(glass_easel_templates(source))))[:5]},
        "glass_easel_structure": glass_easel_structure(source),
        "wxss_broken_imports": broken_imports(source, registered["wxss"])[:20],
        "wxss_broken_import_count": len(broken_imports(source, registered["wxss"])),
        "wxss": {"registered": len(registered["wxss"]), "restored": len(restored(registered["wxss"])),
                 "missing_examples": sorted(set(registered["wxss"]) - set(restored(registered["wxss"])))[:5]},
        "plugin_templates": {kind: {"registered": len(registered["plugin_" + kind]),
                                    "restored": len(restored(plugin_paths(registered["plugin_" + kind])))}
                             for kind in ("wxml", "wxss")},
        "pages": {"declared": len(pages), **{ext.lstrip("."): count for ext, count in page_files.items()}},
        "artifacts": len(manifest["artifacts"]),
        "confidence": confidence,
        "diagnostics": diagnostics,
    }


def ratio(restored, registered):
    return f"{restored}/{registered}" + (f" ({restored / registered:.0%})" if registered else "")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin", required=True, help="WxLocated executable or wxlocated shim")
    parser.add_argument("--out", required=True, help="directory for exports and results (outside the repo)")
    parser.add_argument("--app", action="append", help="limit to these AppIDs (repeatable)")
    parser.add_argument("--label", default="run", help="name for this run's result file")
    args = parser.parse_args()

    os.makedirs(args.out, exist_ok=True)
    listing = json.loads(subprocess.run([args.bin, "list", "--compact"], capture_output=True, text=True, check=True).stdout)
    apps = [app["app_id"] for app in listing["miniapps"]]
    if args.app:
        apps = [app for app in apps if app in args.app]

    exports = os.path.join(args.out, "exports")
    results = []
    for index, app_id in enumerate(apps, 1):
        started = time.time()
        run = subprocess.run([args.bin, "export", app_id, "--out", exports, "--force", "--no-raw", "--compact"],
                             capture_output=True, text=True)
        elapsed = time.time() - started
        try:
            output = json.loads(run.stdout)
        except ValueError:
            output = {"error": {"message": run.stdout[-300:] or run.stderr[-300:]}}
        if "error" in output:
            results.append({"app_id": app_id, "error": output["error"]["message"], "seconds": round(elapsed, 2)})
        elif not output.get("source"):
            material = json.load(open(output["material"]))
            reason = "; ".join(issue["impact"] for issue in material["main_package"]["issues"]) or "没有重建源码"
            results.append({"app_id": app_id, "name": material["metadata"].get("app_name"),
                            "error": f"{material['completeness']}: {reason}", "seconds": round(elapsed, 2)})
        else:
            results.append(measure(output["root"], elapsed))
        row = results[-1]
        status = row.get("error") or f"wxml {ratio(row['wxml']['restored'], row['wxml']['registered'])} (结构 {ratio(row['wxml_structure']['exact'], row['wxml_structure']['compared'])}) · wxss {ratio(row['wxss']['restored'], row['wxss']['registered'])}"
        print(f"[{index}/{len(apps)}] {app_id} {row.get('name') or ''}: {status}", file=sys.stderr)

    ok = [row for row in results if "error" not in row]
    totals = {
        "apps": len(results),
        "failed": len(results) - len(ok),
        "wxml": [sum(r["wxml"]["restored"] for r in ok), sum(r["wxml"]["registered"] for r in ok)],
        "plugin_wxml": [sum(r["plugin_templates"]["wxml"]["restored"] for r in ok),
                        sum(r["plugin_templates"]["wxml"]["registered"] for r in ok)],
        "plugin_wxss": [sum(r["plugin_templates"]["wxss"]["restored"] for r in ok),
                        sum(r["plugin_templates"]["wxss"]["registered"] for r in ok)],
        "wxml_structure": [sum(r["wxml_structure"]["exact"] for r in ok),
                           sum(r["wxml_structure"]["compared"] for r in ok)],
        "glass_easel": [sum(r["glass_easel"]["restored"] for r in ok),
                        sum(r["glass_easel"]["registered"] for r in ok)],
        "glass_easel_structure": [sum(r["glass_easel_structure"]["exact"] for r in ok),
                                  sum(r["glass_easel_structure"]["compared"] for r in ok)],
        "wxss": [sum(r["wxss"]["restored"] for r in ok), sum(r["wxss"]["registered"] for r in ok)],
        "wxss_broken_imports": sum(r["wxss_broken_import_count"] for r in ok),
        "pages_declared": sum(r["pages"]["declared"] for r in ok),
        "pages_with": {ext: sum(r["pages"][ext] for r in ok) for ext in ("js", "json", "wxml", "wxss")},
        "conflicts": sum(v for r in ok for k, v in r["diagnostics"].items() if "冲突" in k),
        "degraded_files": sum(r["confidence"].get("中置信", 0) + r["confidence"].get("低置信", 0) for r in ok),
        "seconds": {"total": round(sum(r["seconds"] for r in results), 1),
                    "max": max((r["seconds"] for r in results), default=0)},
    }
    report = {"label": args.label, "generated_at": time.strftime("%Y-%m-%dT%H:%M:%S"), "totals": totals, "apps": results}
    path = os.path.join(args.out, f"benchmark-{args.label}.json")
    json.dump(report, open(path, "w"), ensure_ascii=False, indent=2)

    print(f"\n== {args.label}: {totals['apps']} 个小程序（失败 {totals['failed']}）")
    print(f"WXML 模板恢复  {ratio(*totals['wxml'])}")
    print(f"WXML 元素数一致 {ratio(*totals['wxml_structure'])}")
    print(f"glass-easel 模板 {ratio(*totals['glass_easel'])} · 元素数一致 {ratio(*totals['glass_easel_structure'])}")
    print(f"WXSS 样式恢复  {ratio(*totals['wxss'])} · 无法解析的 @import {totals['wxss_broken_imports']}")
    print(f"插件模板 wxml {ratio(*totals['plugin_wxml'])} · wxss {ratio(*totals['plugin_wxss'])}")
    pages = totals["pages_with"]
    print(f"页面 {totals['pages_declared']} 个：js {pages['js']} · json {pages['json']} · wxml {pages['wxml']} · wxss {pages['wxss']}")
    print(f"重建冲突 {totals['conflicts']} · 降级文件 {totals['degraded_files']} · 总耗时 {totals['seconds']['total']}s（最长 {totals['seconds']['max']}s）")
    print(f"结果：{path}")


if __name__ == "__main__":
    main()
