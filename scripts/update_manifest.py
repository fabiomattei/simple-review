#!/usr/bin/env python3
"""Merge the per-platform *.update.json files into the updater's latest.json.

usage: update_manifest.py <dist dir> <tag> <download base URL>  > latest.json
"""
import datetime
import json
import pathlib
import sys

dist, tag, base = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3].rstrip("/")
platforms = {}
for f in sorted(dist.glob("*.update.json")):
    for platform, info in json.loads(f.read_text()).items():
        platforms[platform] = {"url": f"{base}/{info['file']}", "signature": info["signature"], "format": info["format"]}
manifest = {
    "version": tag.lstrip("v"),
    "notes": f"Simple Review {tag}",
    "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "platforms": platforms,
}
json.dump(manifest, sys.stdout, indent=2)
print()
