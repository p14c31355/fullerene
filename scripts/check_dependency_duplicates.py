#!/usr/bin/env python3
"""Reject unreviewed duplicate dependency versions."""

from __future__ import annotations

import json
import subprocess
import sys
import tomllib
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
POLICY = ROOT / "dependency-duplicates.toml"


def main() -> int:
    policy = tomllib.loads(POLICY.read_text(encoding="utf-8"))["allow"]
    metadata = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1", "--locked"],
            cwd=ROOT,
        ).decode("utf-8")
    )
    versions: dict[str, set[str]] = defaultdict(set)
    for package in metadata["packages"]:
        versions[package["name"]].add(package["version"])

    duplicates = {
        name: sorted(found)
        for name, found in versions.items()
        if len(found) > 1
    }
    failures: list[str] = []
    for name, found in sorted(duplicates.items()):
        allowed = sorted(policy.get(name, []))
        if found != allowed:
            failures.append(
                f"{name}: found {', '.join(found)}; allowed {', '.join(allowed) or '(none)'}"
            )

    stale = sorted(set(policy) - set(duplicates))
    if stale:
        failures.append(
            "stale allow-list entries (remove after dependency convergence): "
            + ", ".join(stale)
        )

    if failures:
        print("Duplicate dependency policy failed:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1

    print(
        "Duplicate dependency policy satisfied: "
        + ", ".join(f"{name}={versions}" for name, versions in duplicates.items())
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
