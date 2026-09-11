#!/usr/bin/env python3
"""Check vault metadata, local links, evidence paths, and recorded model hash."""

from __future__ import annotations

import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote


ROOT = Path(__file__).resolve().parents[1]
VALID_STATUSES = {"accepted", "designed", "observed", "planned"}


def main():
    errors = []
    identifiers = {}
    note_count = 0
    link_count = 0
    paths = sorted(ROOT.rglob("*.md"))
    for path in paths:
        # The build tree is not the repository: it holds fetched third-party
        # sources whose prose is theirs, and nothing in it is a note.
        if path.relative_to(ROOT).parts[0] in (".git", "build"):
            continue
        content = path.read_text(encoding="utf-8")
        if "vault" == path.relative_to(ROOT).parts[0]:
            note_count += 1
            match = re.match(r"\A---\n(.*?)\n---\n", content, re.S)
            if not match:
                errors.append(f"Missing metadata: {path.relative_to(ROOT)}")
            else:
                metadata = dict(re.findall(r"^(\w+):\s*(.+)$", match[1], re.M))
                for field in ("id", "kind", "status"):
                    if field not in metadata:
                        errors.append(f"Missing {field}: {path.relative_to(ROOT)}")
                identifier = metadata.get("id")
                if identifier in identifiers:
                    errors.append(f"Duplicate id: {identifier}")
                identifiers[identifier] = path
                if metadata.get("status") not in VALID_STATUSES:
                    errors.append(f"Unknown status: {path.relative_to(ROOT)}")
        stripped = re.sub(r"^\x60\x60\x60.*?^\x60\x60\x60\s*$", "", content, flags=re.M | re.S)
        for target in re.findall(r"\[[^\]\n]*\]\(([^)\s]+)\)", stripped):
            if re.match(r"^[a-zA-Z][a-zA-Z0-9+.-]*:", target) or target.startswith("#"):
                continue
            local = unquote(target.split("#", 1)[0])
            resolved = (path.parent / local).resolve()
            link_count += 1
            if not resolved.is_relative_to(ROOT):
                errors.append(f"Local link escapes repository: {path.relative_to(ROOT)} -> {target}")
            elif not resolved.exists():
                errors.append(f"Broken link: {path.relative_to(ROOT)} -> {target}")
        if re.search(r"\b(TODO|TBD|FIXME)\b", stripped):
            errors.append(f"Unresolved placeholder: {path.relative_to(ROOT)}")
    manifest_path = ROOT / "vault/evidence/source-manifest.json"
    if not manifest_path.exists():
        errors.append("Missing source manifest")
    else:
        manifest = json.loads(manifest_path.read_text())
        if not re.fullmatch(r"[0-9a-f]{40}", manifest["repository"]["commit"]):
            errors.append("Source commit is not fully pinned")
        for item in manifest["files"]:
            if not re.fullmatch(r"[0-9a-f]{64}", item["sha256"]):
                errors.append(f"Invalid source digest: {item['path']}")
    result_path = ROOT / "research/models/results.json"
    if not result_path.exists():
        errors.append("Missing recorded model results")
    else:
        result = json.loads(result_path.read_text())
        actual = hashlib.sha256((ROOT / "research/models/check_models.py").read_bytes()).hexdigest()
        if result["script_sha256"] != actual:
            errors.append("Recorded model results refer to a different script")
    tracked_conflicts = subprocess.run(
        ["git", "diff", "--check"], cwd=ROOT, text=True, capture_output=True
    )
    if tracked_conflicts.returncode:
        errors.append(tracked_conflicts.stdout + tracked_conflicts.stderr)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(json.dumps({
        "status": "PASS",
        "vault_notes": note_count,
        "local_links_checked": link_count,
        "note_ids": len(identifiers),
        "scope": "metadata, local file links, source digest shape, model script identity",
    }, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
