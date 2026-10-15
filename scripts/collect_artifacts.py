#!/usr/bin/env python3
"""Copy one platform's packages into dist/ with unique names and record its update bundle.

usage: collect_artifacts.py <packages dir> <dist dir> <platform, e.g. macos-aarch64> <tag>

Writes dist/<platform>.update.json describing the update bundle (if one was signed), which
update_manifest.py merges into latest.json.
"""
import json
import pathlib
import shutil
import sys

src, dist, platform, tag = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), sys.argv[3], sys.argv[4]
version = tag.lstrip("v")
dist.mkdir(parents=True, exist_ok=True)

# Update bundle formats per file suffix, most preferred first.
UPDATE_FORMATS = [(".app.tar.gz", "app"), (".AppImage", "appimage"), ("-setup.exe", "nsis"), (".msi", "wix")]
INSTALLERS = (".dmg", ".msi", "-setup.exe", ".AppImage", ".deb", ".app.tar.gz")

update = None
for f in sorted(src.iterdir()):
    if not f.is_file() or f.name.endswith(".sig") or not f.name.endswith(INSTALLERS):
        continue
    suffix = next(s for s in INSTALLERS if f.name.endswith(s))
    name = f"Simple-Review_{version}_{platform}{suffix}"
    shutil.copy2(f, dist / name)
    sig = f.with_name(f.name + ".sig")
    if sig.exists():
        shutil.copy2(sig, dist / (name + ".sig"))
        fmt = next((fmt for s, fmt in UPDATE_FORMATS if f.name.endswith(s)), None)
        rank = [s for s, _ in UPDATE_FORMATS].index(suffix) if fmt else 99
        if fmt and (update is None or rank < update[0]):
            update = (rank, {"file": name, "signature": sig.read_text().strip(), "format": fmt})
    print(f"{f.name} -> {name}")

if update:
    (dist / f"{platform}.update.json").write_text(json.dumps({platform: update[1]}))
