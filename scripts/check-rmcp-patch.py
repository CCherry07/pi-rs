#!/usr/bin/env python3
"""Check pinned rmcp provenance and the exact reviewed source delta, without network access."""
import argparse
import difflib
import hashlib
import os
from pathlib import Path
import tarfile

ROOT = Path(__file__).resolve().parent.parent
VENDOR = ROOT / "vendor/rmcp-3.3.0"
ARCHIVE_SHA256 = "b88db56b8ae316560e9e868b6b978ea940f27cb883323fc90e07435e44158f5c"
PATCHED = {
    "src/transport/streamable_http_client.rs",
    "src/transport/common/reqwest/streamable_http_client.rs",
}
ADDED = {"PI-PATCH.md", "PI-PATCH.diff", "LICENSE-APACHE"}


def check(archive: Path, write_diff: bool) -> None:
    if hashlib.sha256(archive.read_bytes()).hexdigest() != ARCHIVE_SHA256:
        raise ValueError("archive checksum does not match the locked rmcp 3.3.0 source")
    originals = {}
    with tarfile.open(archive, "r:gz") as package:
        for member in package.getmembers():
            if not member.isfile():
                continue
            path = Path(member.name).relative_to("rmcp-3.3.0").as_posix()
            if ".." in Path(path).parts:
                raise ValueError("invalid source archive path")
            originals[path] = package.extractfile(member).read()
    delta = []
    changed = set()
    for path, original in sorted(originals.items()):
        actual = (VENDOR / path).read_bytes()
        if original != actual:
            if path not in PATCHED:
                raise ValueError(f"unexpected vendored change: {path}")
            changed.add(path)
            delta.extend(difflib.unified_diff(
                original.decode().splitlines(keepends=True),
                actual.decode().splitlines(keepends=True),
                fromfile=f"upstream/rmcp-3.3.0/{path}",
                tofile=f"vendor/rmcp-3.3.0/{path}",
            ))
    if changed != PATCHED:
        raise ValueError("expected transport metadata patch is missing")
    extras = {path.relative_to(VENDOR).as_posix() for path in VENDOR.rglob("*") if path.is_file()} - originals.keys()
    if extras - ADDED:
        raise ValueError(f"unexpected vendored files: {sorted(extras - ADDED)}")
    delta = "".join(delta)
    expected = VENDOR / "PI-PATCH.diff"
    if write_diff:
        expected.write_text(delta)
    elif expected.read_text() != delta:
        raise ValueError("vendor delta differs from PI-PATCH.diff; review before updating it")
    print(f"rmcp 3.3.0 checksum verified; {len(originals)} upstream files; exactly two patched source files")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, help="checksum-pinned rmcp-3.3.0.crate")
    parser.add_argument("--write-diff", action="store_true", help="materialize the delta for review")
    args = parser.parse_args()
    archive = args.archive
    if archive is None:
        cargo = Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")))
        candidates = sorted(cargo.glob("registry/cache/*/rmcp-3.3.0.crate"))
        archive = next((path for path in candidates if hashlib.sha256(path.read_bytes()).hexdigest() == ARCHIVE_SHA256), None)
        if archive is None:
            parser.error("pinned archive is not cached; supply --archive (this check never downloads)")
    try:
        check(archive, args.write_diff)
    except (OSError, ValueError, tarfile.TarError) as error:
        parser.exit(1, f"rmcp patch check failed: {error}\n")


if __name__ == "__main__":
    main()
