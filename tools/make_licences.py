"""Write docs/third-party.md from what cargo actually resolved.

Shipping other people's code obliges us to say whose it is and under what
terms, and a hand-maintained list is a list that is wrong. This reads the
resolved dependency graph, so the file cannot drift from the binary.

It also refuses to write a list containing anything copyleft-only. The Editor
is MIT, distributed as a single statically linked executable, and a GPL
dependency would quietly change the terms of the whole thing. Dual-licensed
crates offering a permissive option are fine -- we take that option.

Run: python tools/make_licences.py
"""

import json
import pathlib
import subprocess
import sys

# Licences that are fine to link into an MIT binary. Anything outside this set
# stops the script rather than being written out unnoticed.
PERMISSIVE = {
    "MIT",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "MPL-2.0",  # file-level copyleft; linking is fine, and we modify nothing
    "Unlicense",
    "CC0-1.0",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "BSL-1.0",
    "OFL-1.1",
    "NCSA",
    "0BSD",
}


def alternatives(expression):
    """The individual licences a SPDX-ish expression offers as a choice.

    cargo has accumulated three spellings over the years -- `A OR B`, `A/B`
    and `A AND B` -- and crates in the graph use all of them.
    """
    text = expression.replace("/", " OR ")
    parts = [p.strip(" ()") for p in text.split(" OR ")]
    out = []
    for part in parts:
        # `A AND B` is not a choice: every one of them applies.
        out.extend(bit.strip(" ()") for bit in part.split(" AND "))
    return [p for p in out if p]


def acceptable(expression):
    """True if the crate offers at least one licence we can accept.

    For an `OR`, one permissive option is enough. This deliberately does not
    try to be a full SPDX evaluator: anything it cannot read confidently ends
    up reported rather than approved.
    """
    if not expression:
        return False
    for option in [p.strip() for p in expression.replace("/", " OR ").split(" OR ")]:
        if all(bit.strip(" ()") in PERMISSIVE for bit in option.split(" AND ")):
            return True
    return False


def main():
    root = pathlib.Path(__file__).resolve().parent.parent
    raw = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--all-features"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    meta = json.loads(raw)

    # Workspace crates are ours; they are not third-party anything.
    ours = {p["name"] for p in meta["packages"] if p["id"].startswith("path+")}
    external = sorted(
        (p for p in meta["packages"] if p["name"] not in ours),
        key=lambda p: p["name"].lower(),
    )

    problems = [p for p in external if not acceptable(p.get("license"))]
    if problems:
        print("Refusing to write the list: these are not clearly permissive.")
        for p in problems:
            print(f"  {p['name']} {p['version']}: {p.get('license') or '(none declared)'}")
        return 1

    counts = {}
    for p in external:
        for name in alternatives(p["license"]):
            counts[name] = counts.get(name, 0) + 1

    lines = [
        "# Third-party software",
        "",
        "The Editor is MIT licensed. It is built from the Rust crates listed",
        "below, each under its own terms; where a crate offers a choice, The",
        "Editor takes a permissive option.",
        "",
        "This file is generated from the resolved dependency graph by",
        "`tools/make_licences.py`, so it cannot drift from what actually ships.",
        "Full licence texts are in each crate's source, which `cargo vendor`",
        "will fetch.",
        "",
        f"## {len(external)} crates",
        "",
        "Licences appearing, and how many crates offer each:",
        "",
    ]
    for name, n in sorted(counts.items(), key=lambda kv: (-kv[1], kv[0])):
        lines.append(f"- {name} — {n}")
    lines += ["", "## Every crate", ""]
    for p in external:
        lines.append(f"- {p['name']} {p['version']} — {p['license']}")
    lines.append("")

    out = root / "docs" / "third-party.md"
    out.parent.mkdir(exist_ok=True)
    out.write_text("\n".join(lines), encoding="utf-8", newline="\n")
    print(f"wrote {out.relative_to(root)}: {len(external)} crates")
    return 0


if __name__ == "__main__":
    sys.exit(main())
