#!/usr/bin/env python3
"""Synchronize Pi's checked-in classifier/image catalog without network or credentials."""

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LEGACY = ROOT / "legacy/pi"
SOURCE = LEGACY / "packages/ai/src/providers/data"
TARGET = ROOT / "plugins/providers/pi-plugin-models/src/data/specialized.json"
ORACLE_HEAD = "0ab23c37b074252d04e31f5cfb99491f0613fb80"
PROVIDER_NAMES = {
    "cloudflare-workers-ai": "Cloudflare Workers AI",
    "openai": "OpenAI",
    "opencode": "OpenCode Zen",
    "openrouter": "OpenRouter",
    "typesafe": "TypeSafe",
    "vercel-ai-gateway": "Vercel AI Gateway",
}
# These are the typed fields Rust can validate, retain in its catalog, and route.
# Fail on a new upstream field rather than silently dropping useful metadata.
SUPPORTED_FIELDS = {
    "type",
    "id",
    "name",
    "api",
    "baseUrl",
    "input",
    "inputLimits",
    "output",
    "cost",
    "contextWindow",
    "headers",
    "compat",
}
SOURCE_ONLY_FIELDS = {"provider"}


def git_output(*args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(LEGACY), *args],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def verify_oracle() -> None:
    head = git_output("rev-parse", "HEAD")
    if head != ORACLE_HEAD:
        raise RuntimeError(f"legacy/pi HEAD is {head}, expected fixed oracle {ORACLE_HEAD}")
    dirty = git_output(
        "status",
        "--porcelain",
        "--untracked-files=all",
        "--",
        "packages/ai/src/providers/data",
    )
    if dirty:
        raise RuntimeError(
            "legacy specialized catalog sources are dirty; restore or commit these oracle inputs:\n"
            + dirty
        )


def generate() -> dict[str, object]:
    verify_oracle()

    providers: dict[str, object] = {}
    for path in sorted(SOURCE.glob("*.json")):
        if path.name.startswith("."):
            continue
        provider = path.stem
        models: list[dict[str, object]] = []
        groups = json.loads(path.read_text())
        for entries in groups.values():
            for source in entries.values():
                if source.get("type", "chat") not in {"classifier", "image"}:
                    continue
                unknown = set(source) - SUPPORTED_FIELDS - SOURCE_ONLY_FIELDS
                if unknown:
                    raise RuntimeError(
                        f"{path.relative_to(ROOT)} model {source.get('id')!r} has unsupported fields: "
                        + ", ".join(sorted(unknown))
                    )
                if source.get("provider") != provider:
                    raise RuntimeError(
                        f"{path.relative_to(ROOT)} model {source.get('id')!r} has provider "
                        f"{source.get('provider')!r}, expected {provider!r}"
                    )
                models.append({key: value for key, value in source.items() if key in SUPPORTED_FIELDS})
        if models:
            name = PROVIDER_NAMES.get(provider)
            if name is None:
                raise RuntimeError(f"add a provider name for specialized catalog {provider!r}")
            providers[provider] = {"name": name, "models": models}

    missing = set(PROVIDER_NAMES) - set(providers)
    if missing:
        raise RuntimeError("specialized providers disappeared: " + ", ".join(sorted(missing)))
    return providers


def encoded_catalog() -> str:
    return json.dumps(generate(), ensure_ascii=False, separators=(",", ":")) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail if the checked-in catalog differs")
    args = parser.parse_args()
    content = encoded_catalog()
    if args.check:
        actual = TARGET.read_text() if TARGET.exists() else None
        if actual != content:
            print(f"{TARGET.relative_to(ROOT)} is stale; run {Path(__file__).name}", file=sys.stderr)
            return 1
        print(f"Verified {TARGET.relative_to(ROOT)} against legacy/pi@{ORACLE_HEAD}")
        return 0
    TARGET.parent.mkdir(parents=True, exist_ok=True)
    TARGET.write_text(content)
    catalog = json.loads(content)
    classifiers = sum(
        model["type"] == "classifier"
        for provider in catalog.values()
        for model in provider["models"]
    )
    images = sum(
        model["type"] == "image"
        for provider in catalog.values()
        for model in provider["models"]
    )
    print(f"Synchronized {classifiers} classifiers and {images} image models from legacy/pi@{ORACLE_HEAD}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
