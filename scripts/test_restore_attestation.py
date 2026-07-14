#!/usr/bin/env python3
"""Small contract tests for the closed-beta restore evidence validator."""

from __future__ import annotations

import hashlib
import json
import tempfile
import unittest
from pathlib import Path

from scripts.validate_restore_attestation import (
    EvidenceError,
    sha256_file,
    validate_attestation,
)


class RestoreAttestationTests(unittest.TestCase):
    def test_accepts_linked_encrypted_restore_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifacts = {
                "postgres.dump": b"database",
                "blobs.tar.gz": b"blobs",
                "row-counts.txt": b"accounts 1\nmaterials 1\n",
                "blob-records.txt": b"a" * 64 + b" sha256/aa/object 6\n",
            }
            for name, content in artifacts.items():
                (root / name).write_bytes(content)
            checksum_lines = [
                f"{hashlib.sha256(content).hexdigest()}  {name}"
                for name, content in artifacts.items()
            ]
            (root / "SHA256SUMS").write_text(
                "\n".join(checksum_lines) + "\n", encoding="utf-8"
            )
            manifest = {
                "schema": "lumi.backup.v1",
                "writes_quiesced": True,
                "destination_encrypted": True,
                "drill_only": False,
                "database": "postgres.dump",
                "blobs": "blobs.tar.gz",
                "row_counts": "row-counts.txt",
                "blob_records": "blob-records.txt",
                "checksums": "SHA256SUMS",
            }
            manifest_path = root / "manifest.json"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            restore_output = root / "restore-output.txt"
            restore_output.write_text("restore drill passed\n", encoding="utf-8")
            attestation = {
                "schema": "lumi.restore-attestation.v1",
                "environment": "staging",
                "database_identity": "lumi_staging_restore_drill",
                "operator": "beta-operator@example.test",
                "completed_at": "2026-07-14T09:00:00Z",
                "backup_manifest": {
                    "path": "manifest.json",
                    "sha256": sha256_file(manifest_path),
                },
                "backup_checksums": {
                    "path": "SHA256SUMS",
                    "sha256": sha256_file(root / "SHA256SUMS"),
                },
                "restore_output": {
                    "path": "restore-output.txt",
                    "sha256": sha256_file(restore_output),
                },
                "verification": {
                    "restore_passed": True,
                    "row_counts_match": True,
                    "blob_records_match": True,
                    "row_counts_sha256": sha256_file(root / "row-counts.txt"),
                    "blob_records_sha256": sha256_file(root / "blob-records.txt"),
                },
            }
            attestation_path = root / "attestation.json"
            attestation_path.write_text(json.dumps(attestation), encoding="utf-8")

            validate_attestation(attestation_path)

    def test_rejects_arbitrary_readme(self) -> None:
        with self.assertRaises(EvidenceError):
            validate_attestation(Path(__file__).parents[1] / "README.md")


if __name__ == "__main__":
    unittest.main()
