#!/usr/bin/env bash
# Regenerates the Rust crate table in THIRD_PARTY_LICENSES.md from `cargo metadata`.
#
#   scripts/gen-third-party-licenses.sh          rewrite the table in place
#   scripts/gen-third-party-licenses.sh --check  exit 1 if the table is out of date
#
# The table lists every crate in Cargo.lock (plus the workspace members) with the license
# expression its manifest declares. Only the block between the crate-table markers is touched.
# Needs python3 (present on ubuntu-latest and on the Frame) and cargo.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
doc=$root/THIRD_PARTY_LICENSES.md
check=no
case ${1:-} in
    '') ;;
    --check) check=yes ;;
    *)
        printf 'usage: %s [--check]\n' "${0##*/}" >&2
        exit 2
        ;;
esac

[ -f "$doc" ] || {
    printf 'error: %s not found\n' "$doc" >&2
    exit 1
}
command -v python3 >/dev/null || {
    printf 'error: python3 is required\n' >&2
    exit 1
}

python3 - "$root" "$doc" "$check" <<'PY'
import json
import re
import subprocess
import sys

root, doc, check = sys.argv[1], sys.argv[2], sys.argv[3] == "yes"

meta = json.loads(
    subprocess.run(
        ["cargo", "metadata", "--format-version", "1"],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
)

packages = {p["id"]: p for p in meta["packages"]}
workspace = {p["id"] for p in meta["packages"] if p["source"] is None}
edges = {
    node["id"]: [
        (dep["pkg"], {kind or "normal" for kind in
                     (d.get("kind") for d in dep["dep_kinds"])})
        for dep in node["deps"]
    ]
    for node in meta["resolve"]["nodes"]
}


def reachable(kinds):
    """Package ids reachable from the workspace members over edges of the given kinds."""
    seen = set()
    todo = list(workspace)
    while todo:
        pkg = todo.pop()
        if pkg in seen:
            continue
        seen.add(pkg)
        for child, child_kinds in edges.get(pkg, []):
            if child_kinds & kinds and child not in seen:
                todo.append(child)
    return seen


runtime = reachable({"normal"})
dev = reachable({"normal", "dev"}) - runtime

rows = []
for pkg in meta["packages"]:
    pid = pkg["id"]
    if pid in workspace:
        use = "this project"
    elif pid in runtime:
        use = "runtime"
    elif pid in dev:
        use = "dev"
    else:
        use = "build"
    license = pkg.get("license") or "(none declared)"
    rows.append((pkg["name"], pkg["version"], license, use))

rows.sort(key=lambda r: (r[0].lower(), r[1]))

lines = [
    "| Crate | Version | License | Use |",
    "|---|---|---|---|",
]
for name, version, license, use in rows:
    lines.append(f"| `{name}` | {version} | {license} | {use} |")

block = "\n".join(lines)
begin = "<!-- crate-table:begin -->"
end = "<!-- crate-table:end -->"
with open(doc, encoding="utf-8") as f:
    text = f.read()
pattern = re.compile(
    re.escape(begin) + r".*?" + re.escape(end), re.DOTALL
)
m = pattern.search(text)
if not m:
    sys.exit(f"error: {doc} has no crate-table markers")
updated = pattern.sub(lambda _: f"{begin}\n{block}\n{end}", text, count=1)

if check:
    if updated != text:
        sys.exit("error: THIRD_PARTY_LICENSES.md crate table is out of date; "
                 "run scripts/gen-third-party-licenses.sh")
    print("crate table is up to date")
    sys.exit(0)

if updated != text:
    with open(doc, "w", encoding="utf-8") as f:
        f.write(updated)
    print(f"updated {doc} ({len(rows)} crates)")
else:
    print(f"{doc} is up to date ({len(rows)} crates)")
PY
