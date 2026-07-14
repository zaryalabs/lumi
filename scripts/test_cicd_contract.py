import os
import re
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = ROOT / ".github" / "workflows"


class WorkflowContractTests(unittest.TestCase):
    def test_workflows_never_run_for_pull_requests(self) -> None:
        for path in sorted(WORKFLOWS.glob("*.yml")):
            text = path.read_text(encoding="utf-8")
            self.assertNotRegex(text, r"(?m)^\s*pull_request(?:_target)?:")

    def test_main_release_runs_only_for_main_pushes(self) -> None:
        text = (WORKFLOWS / "main.yml").read_text(encoding="utf-8")
        self.assertRegex(text, r"(?m)^\s{2}push:\s*$")
        self.assertIn("branches: [main]", text)
        self.assertNotIn("workflow_dispatch:", text)
        self.assertIn("runs-on: [self-hosted, zarya-main, geo-eu, ci]", text)

    def test_deploy_is_manual_and_uses_deploy_labels(self) -> None:
        text = (WORKFLOWS / "deploy.yml").read_text(encoding="utf-8")
        self.assertIn("workflow_dispatch:", text)
        self.assertNotRegex(text, r"(?m)^\s{2}push:\s*$")
        self.assertIn("runs-on: [self-hosted, zarya-main, geo-eu, deploy]", text)
        self.assertIn("environment: production-main", text)
        self.assertIn("actions/download-artifact@", text)
        self.assertIn("run-id: ${{ steps.release.outputs.run_id }}", text)
        self.assertNotIn("make release-manifest", text)

    def test_external_actions_are_pinned_by_commit(self) -> None:
        action_pattern = re.compile(r"(?m)^\s*uses:\s*[^\s]+@([^\s#]+)")
        for path in sorted(WORKFLOWS.glob("*.yml")):
            text = path.read_text(encoding="utf-8")
            references = action_pattern.findall(text)
            self.assertTrue(references, path)
            for reference in references:
                self.assertRegex(reference, r"^[0-9a-f]{40}$", path)


class OperationsContractTests(unittest.TestCase):
    def test_required_make_targets_exist(self) -> None:
        text = (ROOT / "ops" / "Makefile").read_text(encoding="utf-8")
        targets = set(re.findall(r"(?m)^([a-z][a-z0-9-]*):", text))
        required = {
            "help",
            "config",
            "ps",
            "status",
            "logs",
            "releases",
            "activate",
            "pull",
            "migrate",
            "up",
            "deploy",
            "smoke",
            "rollback",
            "backup",
            "restore-verify",
        }
        self.assertEqual(set(), required - targets)

    def test_root_wrapper_limits_manifest_source(self) -> None:
        text = (ROOT / "ops" / "lumi-ci-root").read_text(encoding="utf-8")
        self.assertIn(
            "/opt/infra/github-runner/zarya-main/_work/lumi/lumi/builds/releases",
            text,
        )
        self.assertNotIn("docker system prune", text)
        self.assertNotRegex(text, r"(?m)^\s*\.\s+.*manifest")
        self.assertIn("validate-release-manifest.sh", text)

    def test_deploy_uses_ci_manifest_without_resolving_tags_again(self) -> None:
        text = (ROOT / "Makefile").read_text(encoding="utf-8")
        deploy = re.search(r"(?ms)^deploy:.*?(?=^[a-z][a-z0-9-]*:|\Z)", text)
        self.assertIsNotNone(deploy)
        self.assertNotIn("release-manifest", deploy.group(0).splitlines()[0])
        self.assertIn("validate-release-manifest.sh", deploy.group(0))

    def test_sudoers_rule_allows_only_root_deploy_wrapper(self) -> None:
        text = (ROOT / "ops" / "sudoers.example").read_text(encoding="utf-8")
        self.assertIn(
            "Cmnd_Alias LUMI_CI_DEPLOY = /usr/local/sbin/lumi-ci-root-deploy",
            text,
        )
        self.assertIn("runner ALL=(root) NOPASSWD: LUMI_CI_DEPLOY", text)

    def test_release_manifest_resolves_both_images_to_digests(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            temporary = Path(temporary_directory)
            mock_docker = temporary / "docker"
            mock_docker.write_text(
                "#!/bin/sh\n"
                "case \"$*\" in\n"
                "  *lumi-server*) echo '\"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"' ;;\n"
                "  *lumi-web*) echo '\"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"' ;;\n"
                "  *) exit 2 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            mock_docker.chmod(mock_docker.stat().st_mode | stat.S_IXUSR)
            manifest = temporary / "release.env.images"
            environment = os.environ.copy()
            environment.update(
                {
                    "DOCKER": str(mock_docker),
                    "RELEASE_MANIFEST": str(manifest),
                    "RELEASE_ID": "20260714-120000-0123456",
                    "GIT_SHA": "0123456789abcdef0123456789abcdef01234567",
                    "BUILD_TIMESTAMP": "2026-07-14T12:00:00Z",
                    "LUMI_SERVER_IMAGE": "ghcr.io/zaryalabs/lumi-server:sha-test",
                    "LUMI_WEB_IMAGE": "ghcr.io/zaryalabs/lumi-web:sha-test",
                }
            )
            subprocess.run(
                [str(ROOT / "scripts" / "write-release-manifest.sh")],
                cwd=ROOT,
                env=environment,
                check=True,
                capture_output=True,
                text=True,
            )
            text = manifest.read_text(encoding="utf-8")
            self.assertIn("LUMI_RELEASE_SHA=0123456789abcdef0123456789abcdef01234567", text)
            self.assertIn("lumi-server@sha256:" + "a" * 64, text)
            self.assertIn("lumi-web@sha256:" + "b" * 64, text)

    def test_manifest_validator_rejects_shell_content(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            temporary = Path(temporary_directory)
            manifest = temporary / "malicious.env.images"
            sentinel = temporary / "manifest-was-executed"
            manifest.write_text(
                "LUMI_RELEASE_ID=release-1\n"
                "LUMI_RELEASE_SHA=0123456789abcdef0123456789abcdef01234567\n"
                "LUMI_RELEASE_AT=2026-07-14T12:00:00Z\n"
                "LUMI_SERVER_IMAGE=ghcr.io/zaryalabs/lumi-server@sha256:"
                + "a" * 64
                + "\nLUMI_WEB_IMAGE=ghcr.io/zaryalabs/lumi-web@sha256:"
                + "b" * 64
                + f"\n$(touch {sentinel})\n",
                encoding="utf-8",
            )
            result = subprocess.run(
                [str(ROOT / "ops" / "validate-release-manifest.sh"), str(manifest)],
                cwd=ROOT,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(0, result.returncode)
            self.assertFalse(sentinel.exists())


if __name__ == "__main__":
    unittest.main()
