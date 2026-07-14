#!/usr/bin/env python3
"""Validate closed-beta restore evidence and its referenced artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from datetime import datetime
from pathlib import Path
from typing import Any

SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
CHECKSUM_RE = re.compile(r"^([0-9a-f]{64}) [ *](.+)$")


class EvidenceError(ValueError):
    """Raised when restore evidence is incomplete or inconsistent."""


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_json(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"invalid {label}: {error}") from error
    if not isinstance(value, dict):
        raise EvidenceError(f"{label} must be a JSON object")
    return value


def require_string(record: dict[str, Any], key: str) -> str:
    value = record.get(key)
    if not isinstance(value, str) or not value.strip():
        raise EvidenceError(f"{key} must be a non-empty string")
    return value


def require_digest(record: dict[str, Any], key: str) -> str:
    value = require_string(record, key)
    if SHA256_RE.fullmatch(value) is None:
        raise EvidenceError(f"{key} must be a lowercase SHA-256 digest")
    return value


def require_true(record: dict[str, Any], key: str) -> None:
    if record.get(key) is not True:
        raise EvidenceError(f"{key} must be true")


def resolve_evidence_path(root: Path, value: str, label: str) -> Path:
    candidate = Path(value)
    if not candidate.is_absolute():
        candidate = root / candidate
    try:
        candidate = candidate.resolve(strict=True)
    except OSError as error:
        raise EvidenceError(f"missing {label}: {candidate}") from error
    if not candidate.is_file():
        raise EvidenceError(f"{label} is not a file: {candidate}")
    return candidate


def checked_reference(
    attestation: dict[str, Any], root: Path, key: str
) -> tuple[Path, str]:
    reference = attestation.get(key)
    if not isinstance(reference, dict):
        raise EvidenceError(f"{key} must contain path and sha256")
    path = resolve_evidence_path(root, require_string(reference, "path"), key)
    expected = require_digest(reference, "sha256")
    if sha256_file(path) != expected:
        raise EvidenceError(f"{key} SHA-256 mismatch")
    return path, expected


def manifest_artifact(manifest_path: Path, manifest: dict[str, Any], key: str) -> Path:
    name = require_string(manifest, key)
    if Path(name).name != name:
        raise EvidenceError(f"backup manifest {key} must be a plain file name")
    return resolve_evidence_path(manifest_path.parent, name, f"manifest {key}")


def parse_checksums(path: Path) -> dict[str, str]:
    checksums: dict[str, str] = {}
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError) as error:
        raise EvidenceError(f"invalid checksum file: {error}") from error
    for line in lines:
        match = CHECKSUM_RE.fullmatch(line)
        if match is None or Path(match.group(2)).name != match.group(2):
            raise EvidenceError("checksum file contains a malformed entry")
        checksums[match.group(2)] = match.group(1)
    return checksums


def validate_attestation(path: Path) -> None:
    path = path.resolve(strict=True)
    attestation = load_json(path, "restore attestation")
    if attestation.get("schema") != "lumi.restore-attestation.v1":
        raise EvidenceError("unsupported restore attestation schema")

    environment = require_string(attestation, "environment")
    if environment not in {"staging", "production"}:
        raise EvidenceError("environment must be staging or production")
    database_identity = require_string(attestation, "database_identity")
    if not database_identity.endswith("_restore_drill"):
        raise EvidenceError("database_identity must identify a disposable *_restore_drill database")
    operator = require_string(attestation, "operator")
    if len(operator) < 3 or len(operator) > 200:
        raise EvidenceError("operator identity length is invalid")
    completed_at = require_string(attestation, "completed_at")
    try:
        completed = datetime.fromisoformat(completed_at.replace("Z", "+00:00"))
    except ValueError as error:
        raise EvidenceError("completed_at must be an ISO-8601 timestamp") from error
    if completed.tzinfo is None or completed.utcoffset() is None:
        raise EvidenceError("completed_at must include a timezone")

    root = path.parent
    manifest_path, _ = checked_reference(attestation, root, "backup_manifest")
    checksums_path, _ = checked_reference(attestation, root, "backup_checksums")
    restore_output_path, _ = checked_reference(attestation, root, "restore_output")
    manifest = load_json(manifest_path, "backup manifest")
    if manifest.get("schema") != "lumi.backup.v1":
        raise EvidenceError("unsupported backup manifest schema")
    require_true(manifest, "writes_quiesced")
    require_true(manifest, "destination_encrypted")
    if manifest.get("drill_only") is not False:
        raise EvidenceError("closed-beta backup manifest must set drill_only=false")

    manifest_checksums = manifest_artifact(manifest_path, manifest, "checksums")
    if manifest_checksums != checksums_path:
        raise EvidenceError("attested checksum file does not match backup manifest")
    checksums = parse_checksums(checksums_path)
    artifacts: dict[str, Path] = {}
    for key in ("database", "blobs", "row_counts", "blob_records"):
        artifact = manifest_artifact(manifest_path, manifest, key)
        name = artifact.name
        if checksums.get(name) != sha256_file(artifact):
            raise EvidenceError(f"backup checksum mismatch for {name}")
        artifacts[key] = artifact

    verification = attestation.get("verification")
    if not isinstance(verification, dict):
        raise EvidenceError("verification must be an object")
    for key in ("restore_passed", "row_counts_match", "blob_records_match"):
        require_true(verification, key)
    if require_digest(verification, "row_counts_sha256") != sha256_file(
        artifacts["row_counts"]
    ):
        raise EvidenceError("verified row counts digest does not match backup")
    if require_digest(verification, "blob_records_sha256") != sha256_file(
        artifacts["blob_records"]
    ):
        raise EvidenceError("verified blob records digest does not match backup")

    try:
        restore_lines = restore_output_path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError) as error:
        raise EvidenceError(f"invalid restore output: {error}") from error
    if "restore drill passed" not in restore_lines:
        raise EvidenceError("restore output does not contain the success sentinel")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("attestation", type=Path)
    args = parser.parse_args()
    try:
        validate_attestation(args.attestation)
    except (EvidenceError, OSError) as error:
        print(f"restore attestation rejected: {error}", file=sys.stderr)
        return 1
    print("restore attestation validated")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
