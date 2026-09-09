#!/usr/bin/env python3
"""Generate engines/ff11/src/zone_music.rs from LandSandBoat's zone data.

FFXI zone music is server-sent (packet 0x05F), so the client data has no zone → BGM table.
LandSandBoat (GPL-3) keeps one per zone in `data/zones/<name>/zone.yaml`:

    music:
      day:          109
      night:        109
      battle_solo:  101
      battle_party: 103

`sql/zone_settings.sql` maps zone ids to names (`West_Ronfaure`; the directory is the
lower-cased name with `[S]` → `_s`, `'`/`-` removed) and `docs/MusicIDs.txt` names the tracks.
Everything is fetched from raw.githubusercontent.com at a pinned commit; stdlib only.

    python3 tools/ff11_zone_music.py [--sha SHA] [--out engines/ff11/src/zone_music.rs]

The fetched files are cached in `--cache DIR` (default /tmp/ff11_zone_music) so re-runs are
offline.
"""

import argparse
import json
import os
import re
import sys
import urllib.request
from concurrent.futures import ThreadPoolExecutor

DEFAULT_SHA = "06185f5bf2d6683b8f2b4903e49330790af56790"
RAW = "https://raw.githubusercontent.com/LandSandBoat/server/{sha}/{path}"
TREE = "https://api.github.com/repos/LandSandBoat/server/git/trees/{sha}?recursive=1"


def fetch(url, cache_dir):
    key = re.sub(r"[^A-Za-z0-9_.-]", "_", url)
    path = os.path.join(cache_dir, key)
    if os.path.exists(path):
        with open(path, "rb") as f:
            return f.read()
    req = urllib.request.Request(url, headers={"User-Agent": "fflocal-zone-music"})
    with urllib.request.urlopen(req, timeout=60) as r:
        data = r.read()
    os.makedirs(cache_dir, exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)
    return data


def dir_name(name):
    """`Southern_San_dOria_[S]` → `southern_san_doria_s` (matches the data/zones directories)."""
    n = name.lower().replace("[s]", "s").replace("[d]", "d").replace("'", "").replace("-", "_")
    n = re.sub(r"[^a-z0-9]+", "_", n)
    return n.strip("_")


def parse_zone_settings(text):
    out = {}
    for m in re.finditer(r"INSERT INTO `zone_settings` VALUES \((\d+),'[^']*',\d+,'([^']+)'\)", text):
        out[int(m.group(1))] = m.group(2)
    return out


def parse_music(yaml_text):
    """Return {day, night, battle_solo, battle_party} (missing → 0). Handles the block form
    and a flow form `music: { day: 1, ... }`; a zone without `music:` is silence."""
    keys = ("day", "night", "battle_solo", "battle_party")
    vals = {k: 0 for k in keys}
    m = re.search(r"^music:[ \t]*(\{[^}]*\})?[ \t]*(#.*)?$", yaml_text, re.M)
    if not m:
        return vals, False
    if m.group(1):
        body = m.group(1)
    else:
        rest = yaml_text[m.end():]
        lines = []
        for line in rest.splitlines():
            if line.strip() == "" or line.lstrip().startswith("#"):
                continue
            if not line.startswith((" ", "\t")):
                break
            lines.append(line)
        body = "\n".join(lines)
    for k in keys:
        km = re.search(r"\b%s\s*:\s*(\d+)" % k, body)
        if km:
            vals[k] = int(km.group(1))
    return vals, True


def parse_music_ids(text):
    out = {}
    for line in text.splitlines():
        m = re.match(r"^\s*(\d+)\s+(.*?)\s*$", line)
        if not m:
            continue
        mid = int(m.group(1))
        rest = m.group(2)
        # Columns are space-aligned: "Official Name" then "Used for"; split at 2+ spaces.
        parts = re.split(r"\s{2,}", rest)
        name = parts[0].strip()
        if name in ("???", "Unused ID", ""):
            continue
        out[mid] = name
    return out


def rust_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sha", default=DEFAULT_SHA)
    ap.add_argument("--out", default=os.path.join(os.path.dirname(__file__), "..", "engines", "ff11", "src", "zone_music.rs"))
    ap.add_argument("--cache", default="/tmp/ff11_zone_music")
    args = ap.parse_args()
    sha = args.sha
    cache = os.path.join(args.cache, sha)

    settings = parse_zone_settings(fetch(RAW.format(sha=sha, path="sql/zone_settings.sql"), cache).decode())
    names = parse_music_ids(fetch(RAW.format(sha=sha, path="docs/MusicIDs.txt"), cache).decode(errors="replace"))
    tree = json.loads(fetch(TREE.format(sha=sha), cache))
    if tree.get("truncated"):
        sys.exit("git tree listing truncated")
    zone_dirs = {e["path"].split("/")[2] for e in tree["tree"] if e["path"].startswith("data/zones/") and e["path"].endswith("/zone.yaml")}

    rows = []
    missing = []
    wanted = []
    for zid, name in sorted(settings.items()):
        d = dir_name(name)
        if d not in zone_dirs:
            missing.append((zid, name, d))
            continue
        wanted.append((zid, d))

    def load(item):
        zid, d = item
        text = fetch(RAW.format(sha=sha, path=f"data/zones/{d}/zone.yaml"), cache).decode(errors="replace")
        vals, has = parse_music(text)
        return zid, d, vals, has

    with ThreadPoolExecutor(max_workers=8) as ex:
        results = list(ex.map(load, wanted))
    silent = []
    for zid, d, vals, has in sorted(results):
        if not has:
            silent.append((zid, d))
        rows.append((zid, vals["day"], vals["night"], vals["battle_solo"], vals["battle_party"]))

    for zid, name, d in missing:
        print(f"warning: zone {zid} {name}: no data/zones/{d}/zone.yaml", file=sys.stderr)
    for zid, d in silent:
        print(f"note: zone {zid} {d}: no music: entry (silence)", file=sys.stderr)

    used = sorted({m for r in rows for m in r[1:] if m})
    lines = []
    lines.append("//! FFXI zone music table, generated by `tools/ff11_zone_music.py`. Do not edit by hand.")
    lines.append("//!")
    lines.append(f"//! Derived from LandSandBoat/server (GPL-3.0) at commit {sha}:")
    lines.append("//! `data/zones/<name>/zone.yaml` (`music:` day/night/battle_solo/battle_party),")
    lines.append("//! `sql/zone_settings.sql` (zone ids) and `docs/MusicIDs.txt` (track names). Zone BGM is")
    lines.append("//! server-sent in FFXI, so the client's data files carry no such table. 0 = silence.")
    lines.append("")
    lines.append("/// `(zone, day, night, battle_solo, battle_party)` music ids (`music{id:03}.bgw`).")
    lines.append("pub const ZONE_MUSIC: &[(u16, u16, u16, u16, u16)] = &[")
    for r in rows:
        lines.append(f"    ({r[0]}, {r[1]}, {r[2]}, {r[3]}, {r[4]}),")
    lines.append("];")
    lines.append("")
    lines.append("/// Official track names from `docs/MusicIDs.txt` (ids without a known name are omitted).")
    lines.append("pub const MUSIC_NAMES: &[(u16, &str)] = &[")
    for mid in sorted(names):
        lines.append(f"    ({mid}, {rust_str(names[mid])}),")
    lines.append("];")
    lines.append("")
    lines.append("/// Music ids of a zone, if LandSandBoat has an entry for it.")
    lines.append("pub fn zone_music(zone: u16) -> Option<(u16, u16, u16, u16)> {")
    lines.append("    ZONE_MUSIC.iter().find(|r| r.0 == zone).map(|r| (r.1, r.2, r.3, r.4))")
    lines.append("}")
    lines.append("")
    lines.append("/// Track name of a music id.")
    lines.append("pub fn music_name(id: u16) -> Option<&'static str> {")
    lines.append("    MUSIC_NAMES.iter().find(|(i, _)| *i == id).map(|(_, n)| *n)")
    lines.append("}")
    lines.append("")
    lines.append("#[cfg(test)]")
    lines.append("mod tests {")
    lines.append("    use super::*;")
    lines.append("")
    lines.append("    #[test]")
    lines.append("    fn ronfaure() {")
    lines.append("        assert_eq!(zone_music(100), Some((109, 109, 101, 103)));")
    lines.append("        assert_eq!(zone_music(230).map(|m| m.0), Some(107));")
    lines.append("        assert_eq!(music_name(109), Some(\"Ronfaure\"));")
    lines.append("    }")
    lines.append("}")
    lines.append("")
    out = os.path.normpath(args.out)
    with open(out, "w") as f:
        f.write("\n".join(lines))
    print(f"{out}: {len(rows)} zones ({len(silent)} silent, {len(missing)} without yaml), {len(names)} track names, {len(used)} tracks referenced")


if __name__ == "__main__":
    main()
