#!/usr/bin/env python3
"""Sequentially implement Lumi roadmap product epics with two Codex runs each."""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path, PurePosixPath
from typing import Any, Callable, Iterable, Sequence


REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_STATE_ROOT = REPO_ROOT / ".local" / "codex-plan-runs"
ROADMAP_PATH = "docs/tmp-plans/ROADMAP.md"
STATE_VERSION = 2
STAGE_TRAILER = "Lumi-Plan-Epic"
DEFAULT_CODEX_REASONING_EFFORT = "xhigh"
CODEX_REASONING_EFFORTS = frozenset(
    {"minimal", "low", "medium", "high", "xhigh"}
)

Command = tuple[str, ...]


@dataclasses.dataclass(frozen=True)
class Stage:
    stage_id: str
    plan_path: str
    heading: str
    commit_subject: str
    gates: tuple[Command, ...]


# Epic definitions, commit subjects and executable gates are intentionally
# kept together near the start of this file. The order mirrors ROADMAP.md.
DOC_GATE: tuple[Command, ...] = (("make", "c"),)
PG_GATE: tuple[Command, ...] = DOC_GATE + (
    ("make", "pg-t"),
    ("make", "security"),
)
WEB_GATE: tuple[Command, ...] = DOC_GATE + (("make", "web-e2e"),)
PG_WEB_GATE: tuple[Command, ...] = PG_GATE + (("make", "web-e2e"),)
FULL_GATE: tuple[Command, ...] = PG_GATE + (
    ("make", "compatibility"),
    ("make", "web-e2e"),
)
RELEASE_GATE: tuple[Command, ...] = FULL_GATE + (("make", "performance"),)
PROTOTYPE_GATE: tuple[Command, ...] = DOC_GATE + (("make", "prototype-e2e"),)

PLAN_020 = "docs/tmp-plans/0.2.0-ai-plan.md"
PLAN_030 = "docs/archive/0.3.0-learn-plan.md"
PLAN_040 = "docs/archive/0.4.0-notes-and-desk-plan.md"
PLAN_050 = "docs/tmp-plans/0.5.0-social-plan.md"

# Historical micro-stage manifest retained only to explain trailers created
# before the 2026-07-26 migration to product epics. It is not executed.
LEGACY_MICRO_STAGES: tuple[Stage, ...] = (
    Stage(
        "0.2.0/A1",
        PLAN_020,
        "A1. Persistence и общая инфраструктура",
        "feat(ai): add persistence and job runtime",
        PG_GATE,
    ),
    Stage(
        "0.2.0/A2",
        PLAN_020,
        "A2. Explicit source context и context packs",
        "feat(ai): add explicit source context",
        PG_GATE,
    ),
    Stage(
        "0.2.0/A3",
        PLAN_020,
        "A3. BYOK и internal provider",
        "feat(ai): add BYOK provider runtime",
        PG_GATE,
    ),
    Stage(
        "0.2.0/B1",
        PLAN_020,
        "B1. Web shell и mock-driven UX",
        "feat(web): add AI product shell",
        WEB_GATE,
    ),
    Stage(
        "0.2.0/B2",
        PLAN_020,
        "B2. Conversation runtime",
        "feat(ai): add conversation runtime",
        PG_WEB_GATE,
    ),
    Stage(
        "0.2.0/A4",
        PLAN_020,
        "A4. Durable queue и summary execution",
        "feat(ai): add durable task execution",
        PG_GATE,
    ),
    Stage(
        "0.2.0/B3",
        PLAN_020,
        "B3. Reader и summary UX",
        "feat(web): integrate reader AI workflows",
        PG_WEB_GATE,
    ),
    Stage(
        "0.2.0/C1",
        PLAN_020,
        "C1. MCP connection и transport",
        "feat(mcp): add connection transport",
        PG_GATE,
    ),
    Stage(
        "0.2.0/C2",
        PLAN_020,
        "C2. MCP product и AI worker tools",
        "feat(mcp): add AI worker tools",
        FULL_GATE,
    ),
    Stage(
        "0.2.0/C3",
        PLAN_020,
        "C3. Abridged material",
        "feat(ai): add abridged material flow",
        FULL_GATE,
    ),
    Stage(
        "0.2.0/release",
        PLAN_020,
        "Общий финальный этап. Hardening и release",
        "chore(ai): harden 0.2.0 release",
        RELEASE_GATE,
    ),
    Stage(
        "0.3.0/G0",
        PLAN_030,
        "Общий Gate G0. Контракты, ADR и fixtures",
        "docs(learn): freeze learning contracts",
        DOC_GATE,
    ),
    Stage(
        "0.3.0/C0",
        PLAN_030,
        "C0. UX states, routing и prototype",
        "feat(web): prototype learning flows",
        PROTOTYPE_GATE,
    ),
    Stage(
        "0.3.0/G1",
        PLAN_030,
        "Общий Gate G1. Shared learning foundation",
        "feat(learn): add shared learning foundation",
        PG_GATE,
    ),
    Stage(
        "0.3.0/A1",
        PLAN_030,
        "A1. Durable learning CRUD и sessions",
        "feat(learn): add durable sessions",
        PG_GATE,
    ),
    Stage(
        "0.3.0/A2",
        PLAN_030,
        "A2. Completion и immediate recall",
        "feat(learn): add completion recall flow",
        PG_GATE,
    ),
    Stage(
        "0.3.0/C1",
        PLAN_030,
        "C1. Deterministic Reader и session flow",
        "feat(web): add deterministic learning flow",
        PG_WEB_GATE,
    ),
    Stage(
        "0.3.0/A3",
        PLAN_030,
        "A3. FSRS, hints и due projections",
        "feat(learn): add scheduling and hints",
        PG_GATE,
    ),
    Stage(
        "0.3.0/C2",
        PLAN_030,
        "C2. Challenges и scheduling UX",
        "feat(web): add challenges scheduling UX",
        PG_WEB_GATE,
    ),
    Stage(
        "0.3.0/B1",
        PLAN_030,
        "B1. Generation и open-answer evaluation",
        "feat(learn): add AI item generation",
        PG_GATE,
    ),
    Stage(
        "0.3.0/B2",
        PLAN_030,
        "B2. Text explain-back",
        "feat(learn): add text explain-back",
        PG_GATE,
    ),
    Stage(
        "0.3.0/C3",
        PLAN_030,
        "C3. AI drafts и explain-back UX",
        "feat(web): add explain-back UX",
        PG_WEB_GATE,
    ),
    Stage(
        "0.3.0/B3",
        PLAN_030,
        "B3. Audio attachments и transcription",
        "feat(learn): add audio transcription",
        PG_GATE,
    ),
    Stage(
        "0.3.0/C4",
        PLAN_030,
        "C4. Voice browser adapter и transcript UX",
        "feat(web): add voice learning UX",
        PG_WEB_GATE,
    ),
    Stage(
        "0.3.0/A4",
        PLAN_030,
        "A4. MCP learning parity",
        "feat(mcp): add learning tools",
        FULL_GATE,
    ),
    Stage(
        "0.3.0/release",
        PLAN_030,
        "Общий Release Gate",
        "chore(learn): harden 0.3.0 release",
        RELEASE_GATE,
    ),
    Stage(
        "0.4.0/G0",
        PLAN_040,
        "Общий Gate 0. Контракты, spikes и fixtures",
        "docs(notes): freeze record contracts",
        DOC_GATE,
    ),
    Stage(
        "0.4.0/A1",
        PLAN_040,
        "A1. Annotation v2 и миграция",
        "feat(notes): add annotation v2",
        PG_GATE,
    ),
    Stage(
        "0.4.0/A2",
        PLAN_040,
        "A2. Rich Reader",
        "feat(reader): add rich annotations",
        PG_WEB_GATE,
    ),
    Stage(
        "0.4.0/A3",
        PLAN_040,
        "A3. Voice Notes",
        "feat(notes): add voice notes",
        PG_WEB_GATE,
    ),
    Stage(
        "0.4.0/A4",
        PLAN_040,
        "A4. Links и backlinks",
        "feat(notes): add links and backlinks",
        PG_WEB_GATE,
    ),
    Stage(
        "0.4.0/B1",
        PLAN_040,
        "B1. BM25 + fastText foundation",
        "feat(search): add hybrid search foundation",
        FULL_GATE,
    ),
    Stage(
        "0.4.0/B2",
        PLAN_040,
        "B2. Desk projection и surface",
        "feat(desk): add record workspace",
        PG_WEB_GATE,
    ),
    Stage(
        "0.4.0/B3",
        PLAN_040,
        "B3. Search surfaces",
        "feat(search): add product search surfaces",
        PG_WEB_GATE,
    ),
    Stage(
        "0.4.0/B4",
        PLAN_040,
        "B4. MCP Desk/search parity",
        "feat(mcp): add desk and search tools",
        FULL_GATE,
    ),
    Stage(
        "0.4.0/C1",
        PLAN_040,
        "C1. Record context contract",
        "feat(ai): add record context contract",
        PG_GATE,
    ),
    Stage(
        "0.4.0/C2",
        PLAN_040,
        "C2. Record RAG integration",
        "feat(ai): add record RAG flow",
        FULL_GATE,
    ),
    Stage(
        "0.4.0/integration",
        PLAN_040,
        "Gate I. Сквозная интеграция",
        "test(notes): verify record vertical",
        FULL_GATE,
    ),
    Stage(
        "0.4.0/release",
        PLAN_040,
        "Общий Release gate",
        "chore(notes): harden 0.4.0 release",
        RELEASE_GATE,
    ),
    Stage(
        "0.5.0/G0",
        PLAN_050,
        "Этап 0. Общий contract gate",
        "docs(social): freeze community contracts",
        DOC_GATE,
    ),
    Stage(
        "0.5.0/C1",
        PLAN_050,
        "C1. Fixtures и harness",
        "test(social): add matching harness",
        DOC_GATE,
    ),
    Stage(
        "0.5.0/A1",
        PLAN_050,
        "A1. Foundation",
        "feat(social): add community foundation",
        PG_GATE,
    ),
    Stage(
        "0.5.0/B1",
        PLAN_050,
        "B1. Shell и contract fixtures",
        "feat(web): add community shell",
        WEB_GATE,
    ),
    Stage(
        "0.5.0/A2",
        PLAN_050,
        "A2. Spaces и access",
        "feat(social): add spaces and access",
        PG_GATE,
    ),
    Stage(
        "0.5.0/B2",
        PLAN_050,
        "B2. Spaces и membership",
        "feat(web): add spaces membership UX",
        PG_WEB_GATE,
    ),
    Stage(
        "0.5.0/C2",
        PLAN_050,
        "C2. Spaces/access security",
        "test(social): secure spaces access",
        FULL_GATE,
    ),
    Stage(
        "0.5.0/A3",
        PLAN_050,
        "A3. Share и matching",
        "feat(social): add sharing and matching",
        PG_GATE,
    ),
    Stage(
        "0.5.0/B3",
        PLAN_050,
        "B3. Share и material claims",
        "feat(web): add material sharing UX",
        PG_WEB_GATE,
    ),
    Stage(
        "0.5.0/C3",
        PLAN_050,
        "C3. Sharing/matching security",
        "test(social): secure material matching",
        FULL_GATE,
    ),
    Stage(
        "0.5.0/A4",
        PLAN_050,
        "A4. Совместное чтение",
        "feat(social): add shared reading",
        PG_GATE,
    ),
    Stage(
        "0.5.0/B4",
        PLAN_050,
        "B4. Reader social layer",
        "feat(reader): add social layer",
        PG_WEB_GATE,
    ),
    Stage(
        "0.5.0/C4",
        PLAN_050,
        "C4. Shared reading quality",
        "test(social): verify shared reading",
        FULL_GATE,
    ),
    Stage(
        "0.5.0/A5",
        PLAN_050,
        "A5. Chat и activity",
        "feat(social): add chat and activity",
        PG_GATE,
    ),
    Stage(
        "0.5.0/B5",
        PLAN_050,
        "B5. Chat и activity",
        "feat(web): add social activity UX",
        PG_WEB_GATE,
    ),
    Stage(
        "0.5.0/C5",
        PLAN_050,
        "C5. Release hardening",
        "chore(social): harden 0.5.0 release",
        RELEASE_GATE,
    ),
)

STAGES: tuple[Stage, ...] = (
    Stage(
        "0.2.0/E1",
        PLAN_020,
        "Эпик E1. Персональный AI-ассистент",
        "feat(ai): deliver personal AI assistant",
        PG_WEB_GATE,
    ),
    Stage(
        "0.2.0/E2",
        PLAN_020,
        "Эпик E2. AI-задачи и саммари",
        "feat(ai): deliver tasks and summaries",
        PG_WEB_GATE,
    ),
    Stage(
        "0.2.0/E3",
        PLAN_020,
        "Эпик E3. Внешние агенты через MCP",
        "feat(mcp): deliver external agent integration",
        FULL_GATE,
    ),
    Stage(
        "0.2.0/E4",
        PLAN_020,
        "Эпик E4. Производные материалы и выпуск",
        "feat(ai): release derived material workflows",
        RELEASE_GATE,
    ),
    Stage(
        "0.3.0/E1",
        PLAN_030,
        "Эпик E1. Обучение после чтения",
        "feat(learn): deliver post-reading learning",
        PG_WEB_GATE,
    ),
    Stage(
        "0.3.0/E2",
        PLAN_030,
        "Эпик E2. Повторение и Challenges",
        "feat(learn): deliver challenges and scheduling",
        PG_WEB_GATE,
    ),
    Stage(
        "0.3.0/E3",
        PLAN_030,
        "Эпик E3. AI-обучение и explain-back",
        "feat(learn): deliver AI learning workflows",
        PG_WEB_GATE,
    ),
    Stage(
        "0.3.0/E4",
        PLAN_030,
        "Эпик E4. Голосовой контур",
        "feat(learn): deliver voice learning workflows",
        PG_WEB_GATE,
    ),
    Stage(
        "0.3.0/E5",
        PLAN_030,
        "Эпик E5. Learning platform и выпуск",
        "feat(learn): release learning platform",
        RELEASE_GATE,
    ),
    Stage(
        "0.4.0/E1",
        PLAN_040,
        "Эпик E1. Records v2 и Rich Reader",
        "feat(notes): deliver rich reader records",
        PG_WEB_GATE,
    ),
    Stage(
        "0.4.0/E2",
        PLAN_040,
        "Эпик E2. Голосовые записи и связи",
        "feat(notes): deliver voice notes and links",
        PG_WEB_GATE,
    ),
    Stage(
        "0.4.0/E3",
        PLAN_040,
        "Эпик E3. Поисковое ядро",
        "feat(search): deliver hybrid search core",
        FULL_GATE,
    ),
    Stage(
        "0.4.0/E4",
        PLAN_040,
        "Эпик E4. Desk и единый поиск",
        "feat(desk): deliver desk and search surfaces",
        FULL_GATE,
    ),
    Stage(
        "0.4.0/E5",
        PLAN_040,
        "Эпик E5. RAG по записям и выпуск",
        "feat(ai): release record RAG workflows",
        RELEASE_GATE,
    ),
    Stage(
        "0.5.0/E1",
        PLAN_050,
        "Эпик E1. Community Spaces и доступ",
        "feat(social): deliver community spaces",
        FULL_GATE,
    ),
    Stage(
        "0.5.0/E2",
        PLAN_050,
        "Эпик E2. Публикация и сопоставление материалов",
        "feat(social): deliver material sharing",
        FULL_GATE,
    ),
    Stage(
        "0.5.0/E3",
        PLAN_050,
        "Эпик E3. Совместное чтение",
        "feat(social): deliver shared reading",
        FULL_GATE,
    ),
    Stage(
        "0.5.0/E4",
        PLAN_050,
        "Эпик E4. Коммуникации и выпуск",
        "feat(social): release community communications",
        RELEASE_GATE,
    ),
)

BLOCKED_EXACT_NAMES = {
    "auth.json",
    "credentials.json",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "id_rsa",
}
BLOCKED_SUFFIXES = {".key", ".p12", ".pfx", ".pem"}
BLOCKED_PATH_PARTS = {
    ".git",
    ".local",
    ".playwright-cli",
    "node_modules",
    "playwright-report",
    "target",
    "test-results",
}
CONTROL_PLANE_PATHS = {
    ".devcontainer",
    "scripts/execute_plan.py",
    "scripts/test_execute_plan.py",
}


class RunnerError(RuntimeError):
    """An expected, actionable runner failure."""


def run_capture(
    command: Sequence[str],
    *,
    check: bool = True,
    cwd: Path = REPO_ROOT,
) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        command,
        cwd=cwd,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if check and result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise RunnerError(f"Command failed ({shlex.join(command)}): {detail}")
    return result


def git(*args: str, check: bool = True) -> str:
    return run_capture(("git", *args), check=check).stdout.strip()


def current_head() -> str:
    return git("rev-parse", "HEAD")


def current_branch() -> str:
    return run_capture(
        ("git", "symbolic-ref", "--quiet", "--short", "HEAD"),
        check=False,
    ).stdout.strip()


def worktree_is_clean() -> bool:
    return not git("status", "--porcelain=v1", "--untracked-files=all")


def changed_paths(*, cached: bool = False) -> list[str]:
    diff_args = ["diff", "--name-only", "-z"]
    if cached:
        diff_args.append("--cached")
    else:
        diff_args.append("HEAD")
    tracked = run_capture(("git", *diff_args)).stdout.split("\0")
    untracked: list[str] = []
    if not cached:
        untracked = run_capture(
            ("git", "ls-files", "--others", "--exclude-standard", "-z")
        ).stdout.split("\0")
    return sorted({path for path in (*tracked, *untracked) if path})


def blocked_path_reason(path_text: str) -> str | None:
    path = PurePosixPath(path_text)
    if path_text in CONTROL_PLANE_PATHS or (
        path.parts and path.parts[0] in CONTROL_PLANE_PATHS
    ):
        return "automation control-plane path"
    if any(part in BLOCKED_PATH_PARTS for part in path.parts):
        return "generated, private or repository-internal path"
    name = path.name
    if name in BLOCKED_EXACT_NAMES:
        return "credential filename"
    if name.startswith(".env") and not name.endswith(".example"):
        return "environment file"
    if path.suffix.lower() in BLOCKED_SUFFIXES:
        return "private-key or certificate file"
    return None


def validate_changed_paths(paths: Iterable[str]) -> None:
    blocked = [
        f"{path}: {reason}"
        for path in paths
        if (reason := blocked_path_reason(path)) is not None
    ]
    if blocked:
        joined = "\n  ".join(blocked)
        raise RunnerError(f"Refusing to commit sensitive/generated paths:\n  {joined}")


def stage_by_id(stage_id: str) -> Stage:
    for stage in STAGES:
        if stage.stage_id == stage_id:
            return stage
    raise RunnerError(f"Unknown stage: {stage_id}")


def validate_stage_definitions() -> None:
    ids = [stage.stage_id for stage in STAGES]
    subjects = [stage.commit_subject for stage in STAGES]
    if len(ids) != len(set(ids)):
        raise RunnerError("Stage identifiers must be unique")
    if len(subjects) != len(set(subjects)):
        raise RunnerError("Commit subjects must be unique")
    if not STAGES or STAGES[0].stage_id != "0.2.0/E1":
        raise RunnerError("The configured roadmap must start at current epic 0.2.0/E1")

    roadmap = (REPO_ROOT / ROADMAP_PATH).read_text(encoding="utf-8")
    release_markers = {
        "0.2.0": "### 1. Lumi 0.2.0",
        "0.3.0": "### 2. Lumi 0.3.0",
        "0.4.0": "### 3. Lumi 0.4.0",
        "0.5.0": "### 4. Lumi 0.5.0",
    }
    previous_roadmap_offset = -1
    for stage in STAGES:
        plan = REPO_ROOT / stage.plan_path
        if not plan.is_file():
            raise RunnerError(f"Missing plan for {stage.stage_id}: {stage.plan_path}")
        if stage.heading not in plan.read_text(encoding="utf-8"):
            raise RunnerError(
                f"Heading for {stage.stage_id} not found in {stage.plan_path}: "
                f"{stage.heading}"
            )
        release = stage.stage_id.split("/", maxsplit=1)[0]
        roadmap_offset = roadmap.find(release_markers[release])
        if roadmap_offset < previous_roadmap_offset:
            raise RunnerError(f"Stage order diverges from {ROADMAP_PATH}: {stage.stage_id}")
        if roadmap_offset < 0:
            raise RunnerError(f"Release {release} is missing from {ROADMAP_PATH}")
        previous_roadmap_offset = roadmap_offset
        if not stage.gates:
            raise RunnerError(f"Stage {stage.stage_id} has no executable gate")
        for command in stage.gates:
            if not command or any(not part for part in command):
                raise RunnerError(f"Stage {stage.stage_id} has an invalid gate command")


def configured_releases() -> tuple[str, ...]:
    return tuple(
        dict.fromkeys(
            stage.stage_id.split("/", maxsplit=1)[0] for stage in STAGES
        )
    )


def select_stages(
    from_stage: str | None,
    only_stage: str | None,
    release: str | None = None,
    *,
    completed_stage_ids: set[str] | None = None,
) -> list[Stage]:
    if only_stage:
        return [stage_by_id(only_stage)]
    if from_stage:
        start = stage_by_id(from_stage)
        index = STAGES.index(start)
        return list(STAGES[index:])
    if not release:
        return list(STAGES)

    release_stages = [
        stage
        for stage in STAGES
        if stage.stage_id.split("/", maxsplit=1)[0] == release
    ]
    if not release_stages:
        available = ", ".join(configured_releases())
        raise RunnerError(f"Unknown release: {release}. Available releases: {available}")

    if completed_stage_ids is None:
        completed_stage_ids = {
            stage.stage_id
            for stage in release_stages
            if stage_commit(stage.stage_id) is not None
        }
    for index, stage in enumerate(release_stages):
        if stage.stage_id not in completed_stage_ids:
            return release_stages[index:]
    return []


def stage_commit(stage_id: str) -> str | None:
    result = run_capture(
        (
            "git",
            "log",
            "--format=%H",
            "--fixed-strings",
            f"--grep={STAGE_TRAILER}: {stage_id}",
            "-n",
            "1",
        )
    )
    return result.stdout.strip() or None


def command_text(command: Sequence[str]) -> str:
    return shlex.join(command)


def gate_text(stage: Stage) -> str:
    return "\n".join(f"- `{command_text(command)}`" for command in stage.gates)


def implementation_prompt(stage: Stage) -> str:
    return f"""\
Реализуй только продуктовый эпик `{stage.stage_id}` единого плана Lumi.

Источник эпика:
- план: `{stage.plan_path}`;
- заголовок: `{stage.heading}`;
- порядок: `{ROADMAP_PATH}`.

Сначала полностью прочитай AGENTS.md, README.md, указанный эпик, его outcome,
внутренние workstreams, gate и
связанные канонические документы. Реализуй production-код, migrations, tests и
документацию, необходимые для полного закрытия эпика. Доведи изменения до
описанного gate, а не до частичной компиляции. Не ослабляй существующие
проверки и не подменяй production implementation заглушками.

Минимальные команды внешнего gate, которые после тебя повторит runner:
{gate_text(stage)}

Запускай нужные проверки самостоятельно. Если gate проходит, отметь
выполненные пункты эпика и его результат в подробном плане и `{ROADMAP_PATH}`.
Не отмечай следующий эпик. Не переименовывай и не удаляй файлы планов или
заголовки эпиков: они являются контрактом активного runner.

Ограничения:
- не переходи к следующему эпику;
- не выполняй `git commit`, `git push`, `git reset`, `git checkout`, rebase,
  amend и другие операции с историей;
- не удаляй и не обходи проверки ради зелёного результата;
- не изменяй `.devcontainer/`, `scripts/execute_plan.py` и его tests;
- не проси подтверждений: работай автономно в пределах контейнера и
  репозитория;
- в конце оставь рабочее дерево незакоммиченным.
"""


def audit_prompt(stage: Stage) -> str:
    return f"""\
В рабочем дереве только что реализован продуктовый эпик `{stage.stage_id}` из
`{stage.plan_path}`, заголовок `{stage.heading}`.

Проведи независимый аудит результата. Начни с `git status`, `git diff --stat`
и полного релевантного diff, затем заново сверь реализацию со всем эпиком, его
outcome, внутренними workstreams, gate, AGENTS.md и связанными каноническими
документами. Найди и исправь
пропущенные требования, незавершённые ветки, слабые invariants, migration и
ownership ошибки, недостаточные tests, документацию и capability rollout.
Не ограничивайся обзором: внеси необходимые исправления.

Минимальные команды внешнего gate, которые после тебя повторит runner:
{gate_text(stage)}

Запусти релевантные проверки. Только если gate действительно закрыт, обнови
чек-лист/результат эпика в подробном плане и `{ROADMAP_PATH}`. Не отмечай и не
реализуй следующий эпик и не переходи к нему. Не переименовывай и не удаляй
файлы планов или заголовки эпиков: они являются контрактом активного runner.

Ограничения:
- не выполняй `git commit`, `git push`, `git reset`, `git checkout`, rebase,
  amend и другие операции с историей;
- не ослабляй, не удаляй и не обходи проверки;
- не изменяй `.devcontainer/`, `scripts/execute_plan.py` и его tests;
- не проси подтверждений: работай автономно в пределах контейнера и
  репозитория;
- оставь итоговые изменения незакоммиченными.
"""


def utc_timestamp() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def atomic_write_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        dir=path.parent,
        prefix=f".{path.name}.",
        delete=False,
    ) as temp:
        json.dump(value, temp, ensure_ascii=False, indent=2, sort_keys=True)
        temp.write("\n")
        temp_path = Path(temp.name)
    os.replace(temp_path, path)


def load_state(state_file: Path) -> dict[str, Any]:
    try:
        state = json.loads(state_file.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise RunnerError(f"Cannot read checkpoint {state_file}: {error}") from error
    if state.get("version") != STATE_VERSION:
        raise RunnerError(
            f"Unsupported checkpoint version: {state.get('version')!r}"
        )
    return state


def save_state(state_file: Path, state: dict[str, Any]) -> None:
    state["updated_at"] = dt.datetime.now(dt.timezone.utc).isoformat()
    atomic_write_json(state_file, state)


def require_commands(commands: Iterable[str]) -> None:
    missing = sorted(command for command in set(commands) if shutil.which(command) is None)
    if missing:
        raise RunnerError(f"Missing required commands: {', '.join(missing)}")


def preflight(*, require_clean: bool, require_auth: bool) -> None:
    if os.environ.get("LUMI_CODEX_CONTAINER") != "1":
        raise RunnerError(
            "This runner is restricted to the Lumi devcontainer. "
            "Open .devcontainer/devcontainer.json and retry inside it."
        )
    require_commands(("codex", "git", "make", "python3"))
    if git("rev-parse", "--show-toplevel") != str(REPO_ROOT):
        raise RunnerError(f"Run the script from the Lumi repository: {REPO_ROOT}")
    if not current_branch():
        raise RunnerError("Detached HEAD is not supported")
    git_name = run_capture(("git", "config", "--get", "user.name"), check=False)
    git_email = run_capture(("git", "config", "--get", "user.email"), check=False)
    if not git_name.stdout.strip() or not git_email.stdout.strip():
        raise RunnerError(
            "Git author is not configured in the devcontainer. Set `git config "
            "--local user.name ...` and `git config --local user.email ...`."
        )
    if require_clean and not worktree_is_clean():
        raise RunnerError(
            "Fresh execution requires a clean worktree. Commit or stash existing "
            "changes before starting the plan."
        )
    help_text = run_capture(("codex", "exec", "--help")).stdout
    expected_flags = (
        "--config",
        "--dangerously-bypass-approvals-and-sandbox",
        "--dangerously-bypass-hook-trust",
        "--ephemeral",
        "--ignore-user-config",
        "--output-last-message",
    )
    missing_flags = [flag for flag in expected_flags if flag not in help_text]
    if missing_flags:
        raise RunnerError(
            "Installed Codex CLI is incompatible; missing flags: "
            + ", ".join(missing_flags)
        )
    if require_auth:
        status = run_capture(("codex", "login", "status"), check=False)
        if status.returncode != 0:
            detail = status.stderr.strip() or status.stdout.strip()
            raise RunnerError(
                "Codex is not authenticated inside the devcontainer. "
                f"Run `codex login --device-auth`. Details: {detail}"
            )


def codex_reasoning_effort() -> str:
    effort = os.environ.get(
        "LUMI_CODEX_REASONING_EFFORT",
        DEFAULT_CODEX_REASONING_EFFORT,
    ).strip()
    if effort not in CODEX_REASONING_EFFORTS:
        allowed = ", ".join(sorted(CODEX_REASONING_EFFORTS))
        raise RunnerError(
            "Unsupported LUMI_CODEX_REASONING_EFFORT "
            f"{effort!r}; expected one of: {allowed}"
        )
    return effort


def print_codex_configuration() -> None:
    model = os.environ.get("LUMI_CODEX_MODEL") or "<Codex CLI default>"
    print(f"Codex configuration: model={model}, reasoning={codex_reasoning_effort()}")


def stream_process(
    command: Sequence[str],
    *,
    log_path: Path,
    stdin_text: str | None = None,
    display_line: Callable[[str], str | None] | None = None,
    log_command: bool = True,
) -> int:
    log_path.parent.mkdir(parents=True, exist_ok=True)
    with log_path.open("a", encoding="utf-8") as log:
        if log_command:
            log.write(f"$ {command_text(command)}\n")
            log.flush()
        process = subprocess.Popen(
            command,
            cwd=REPO_ROOT,
            env=os.environ.copy(),
            stdin=subprocess.PIPE if stdin_text is not None else subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        try:
            if stdin_text is not None:
                assert process.stdin is not None
                process.stdin.write(stdin_text)
                process.stdin.close()
            assert process.stdout is not None
            try:
                for line in process.stdout:
                    log.write(line)
                    log.flush()
                    rendered = display_line(line) if display_line is not None else line
                    if rendered:
                        sys.stdout.write(rendered)
                        if not rendered.endswith("\n"):
                            sys.stdout.write("\n")
                        sys.stdout.flush()
            finally:
                process.stdout.close()
            return process.wait()
        except KeyboardInterrupt:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
            raise


def codex_item_text(item: dict[str, Any]) -> str:
    for field in ("text", "message", "aggregated_output", "output"):
        value = item.get(field)
        if isinstance(value, str) and value:
            return value
    return ""


def format_codex_event(line: str) -> str | None:
    """Render one Codex JSONL event for a human without dropping unknown events."""
    try:
        event = json.loads(line)
    except json.JSONDecodeError:
        return line
    if not isinstance(event, dict):
        return f"[codex:event] {json.dumps(event, ensure_ascii=False)}\n"

    event_type = event.get("type", "unknown")
    if event_type == "thread.started":
        thread_id = event.get("thread_id", "unknown")
        return f"[codex] thread started: {thread_id}\n"
    if event_type == "turn.started":
        return "[codex] turn started\n"
    if event_type == "turn.completed":
        usage = event.get("usage")
        if not isinstance(usage, dict):
            return "[codex] turn completed\n"
        fields = (
            ("input", usage.get("input_tokens")),
            ("cached", usage.get("cached_input_tokens")),
            ("output", usage.get("output_tokens")),
            ("reasoning", usage.get("reasoning_output_tokens")),
        )
        summary = ", ".join(
            f"{label}={value}" for label, value in fields if value is not None
        )
        suffix = f": {summary}" if summary else ""
        return f"[codex] turn completed{suffix}\n"
    if event_type in {"error", "turn.failed"}:
        error = event.get("error", event.get("message", "unknown error"))
        if isinstance(error, dict):
            error = error.get("message", json.dumps(error, ensure_ascii=False))
        return f"[codex:error] {error}\n"

    if event_type in {"item.started", "item.updated", "item.completed"}:
        item = event.get("item")
        if not isinstance(item, dict):
            return f"[codex:{event_type}] {json.dumps(event, ensure_ascii=False)}\n"
        item_type = item.get("type", "unknown")
        lifecycle = event_type.removeprefix("item.")

        if item_type == "agent_message":
            text = codex_item_text(item)
            return f"[codex]\n{text.rstrip()}\n" if text else None
        if item_type == "reasoning":
            text = codex_item_text(item)
            return f"[codex:reasoning]\n{text.rstrip()}\n" if text else None
        if item_type == "error":
            message = codex_item_text(item) or "unknown item error"
            return f"[codex:error] {message.rstrip()}\n"
        if item_type == "command_execution":
            command = item.get("command", "<unknown command>")
            if lifecycle == "started":
                return f"[codex:command] $ {command}\n"
            output = item.get("aggregated_output")
            result = ""
            if isinstance(output, str) and output:
                result = output
                if not result.endswith("\n"):
                    result += "\n"
            status = item.get("status", lifecycle)
            exit_code = item.get("exit_code")
            exit_text = f", exit={exit_code}" if exit_code is not None else ""
            return f"{result}[codex:command] {status}{exit_text}\n"

        text = codex_item_text(item)
        if text:
            return f"[codex:{item_type}/{lifecycle}]\n{text.rstrip()}\n"
        details = json.dumps(item, ensure_ascii=False, sort_keys=True)
        return f"[codex:{item_type}/{lifecycle}] {details}\n"

    return f"[codex:{event_type}] {json.dumps(event, ensure_ascii=False)}\n"


def codex_command(last_message_path: Path) -> list[str]:
    command = [
        "codex",
        "exec",
        "--dangerously-bypass-approvals-and-sandbox",
        "--dangerously-bypass-hook-trust",
        "--ephemeral",
        "--ignore-user-config",
        "--config",
        f'model_reasoning_effort="{codex_reasoning_effort()}"',
        "--json",
        "--color",
        "never",
        "--output-last-message",
        str(last_message_path),
        "--cd",
        str(REPO_ROOT),
    ]
    model = os.environ.get("LUMI_CODEX_MODEL")
    if model:
        command.extend(("--model", model))
    command.append("-")
    return command


def run_codex_phase(stage: Stage, phase: str, stage_dir: Path) -> None:
    if phase == "implementation":
        prompt = implementation_prompt(stage)
    elif phase == "audit":
        prompt = audit_prompt(stage)
    else:
        raise RunnerError(f"Unknown Codex phase: {phase}")
    prompt_path = stage_dir / f"{phase}.prompt.md"
    prompt_path.write_text(prompt, encoding="utf-8")
    command = codex_command(stage_dir / f"{phase}.last-message.md")
    (stage_dir / f"{phase}.command.txt").write_text(
        f"{command_text(command)}\n",
        encoding="utf-8",
    )
    exit_code = stream_process(
        command,
        log_path=stage_dir / f"{phase}.jsonl",
        stdin_text=prompt,
        display_line=format_codex_event,
        log_command=False,
    )
    if exit_code != 0:
        raise RunnerError(
            f"Codex {phase} failed for {stage.stage_id} with exit code "
            f"{exit_code}. See {stage_dir}"
        )


def run_gates(stage: Stage, stage_dir: Path) -> None:
    for index, command in enumerate(stage.gates, start=1):
        print(f"\n[gate {index}/{len(stage.gates)}] {command_text(command)}")
        exit_code = stream_process(
            command,
            log_path=stage_dir / f"gate-{index:02d}.log",
        )
        if exit_code != 0:
            raise RunnerError(
                f"Gate failed for {stage.stage_id}: {command_text(command)}. "
                f"See {stage_dir / f'gate-{index:02d}.log'}"
            )


def validate_stage_diff(stage: Stage) -> list[str]:
    paths = changed_paths()
    if not paths:
        raise RunnerError(f"Stage {stage.stage_id} produced no changes")
    validate_changed_paths(paths)
    required_docs = {stage.plan_path, ROADMAP_PATH}
    missing_docs = sorted(required_docs.difference(paths))
    if missing_docs:
        raise RunnerError(
            "Stage must update its checklist/result and roadmap before commit; "
            f"missing changes: {', '.join(missing_docs)}"
        )
    run_capture(("git", "diff", "--check"))
    return paths


def commit_stage(stage: Stage) -> str:
    run_capture(("git", "add", "--all", "--", "."))
    staged = changed_paths(cached=True)
    if not staged:
        raise RunnerError(f"Nothing staged for {stage.stage_id}")
    validate_changed_paths(staged)
    run_capture(("git", "diff", "--cached", "--check"))
    result = run_capture(
        (
            "git",
            "commit",
            "-m",
            stage.commit_subject,
            "-m",
            f"{STAGE_TRAILER}: {stage.stage_id}",
        ),
        check=False,
    )
    sys.stdout.write(result.stdout)
    sys.stderr.write(result.stderr)
    if result.returncode != 0:
        raise RunnerError(
            f"Commit failed for {stage.stage_id}. Fix the reported issue and "
            "run with --resume."
        )
    commit = current_head()
    if not worktree_is_clean():
        raise RunnerError(
            f"Commit {commit} succeeded but hooks left a dirty worktree. "
            "Do not start the next stage; inspect the changes manually."
        )
    return commit


def print_stage(stage: Stage) -> None:
    print(f"{stage.stage_id:18} {stage.commit_subject}")
    print(f"{'':18} plan: {stage.plan_path} :: {stage.heading}")
    for command in stage.gates:
        print(f"{'':18} gate: {command_text(command)}")


def create_state(selected: Sequence[Stage], state_root: Path) -> tuple[Path, dict[str, Any]]:
    state_file = state_root / "state.json"
    if state_file.exists():
        raise RunnerError(
            f"An unfinished checkpoint exists at {state_file}; use --resume."
        )
    run_id = utc_timestamp()
    run_dir = state_root / run_id
    suffix = 1
    while run_dir.exists():
        run_dir = state_root / f"{run_id}-{suffix}"
        suffix += 1
    run_dir.mkdir(parents=True)
    state: dict[str, Any] = {
        "version": STATE_VERSION,
        "run_id": run_dir.name,
        "run_dir": str(run_dir),
        "branch": current_branch(),
        "baseline_head": current_head(),
        "selected_stage_ids": [stage.stage_id for stage in selected],
        "current_index": 0,
        "stages": {},
        "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
    }
    save_state(state_file, state)
    return state_file, state


def verify_resume_state(state: dict[str, Any]) -> list[Stage]:
    if current_branch() != state["branch"]:
        raise RunnerError(
            f"Checkpoint belongs to branch {state['branch']}, current branch is "
            f"{current_branch()}"
        )
    selected = [stage_by_id(stage_id) for stage_id in state["selected_stage_ids"]]
    if state["current_index"] > len(selected):
        raise RunnerError("Checkpoint current_index is invalid")
    return selected


def run_stage(
    stage: Stage,
    *,
    state: dict[str, Any],
    state_file: Path,
) -> None:
    records = state["stages"]
    record = records.setdefault(
        stage.stage_id,
        {
            "phase": "pending",
            "start_head": current_head(),
            "stage_dir": str(
                Path(state["run_dir"]) / stage.stage_id.replace("/", "__")
            ),
        },
    )
    stage_dir = Path(record["stage_dir"])
    stage_dir.mkdir(parents=True, exist_ok=True)

    if record["phase"] == "committed":
        if stage_commit(stage.stage_id) != record.get("commit"):
            raise RunnerError(f"Committed checkpoint mismatch for {stage.stage_id}")
        return

    if record["phase"] != "pending" and current_head() != record["start_head"]:
        raise RunnerError(
            f"HEAD changed outside the runner during {stage.stage_id}; expected "
            f"{record['start_head']}, got {current_head()}"
        )

    if record["phase"] == "pending":
        existing_commit = stage_commit(stage.stage_id)
        if existing_commit:
            record.update(phase="committed", commit=existing_commit, recovered=True)
            save_state(state_file, state)
            return
        if not worktree_is_clean():
            raise RunnerError(
                f"Stage {stage.stage_id} must start from a clean worktree"
            )
        record["start_head"] = current_head()
        record["phase"] = "implementation_started"
        save_state(state_file, state)

    if record["phase"] in {"implementation_started", "implementation_failed"}:
        if current_head() != record["start_head"]:
            raise RunnerError(f"HEAD changed during {stage.stage_id}")
        print(f"\n=== {stage.stage_id}: implementation ===")
        try:
            run_codex_phase(stage, "implementation", stage_dir)
        except RunnerError:
            record["phase"] = "implementation_failed"
            save_state(state_file, state)
            raise
        if current_head() != record["start_head"]:
            raise RunnerError(
                f"Codex changed git history during {stage.stage_id} implementation"
            )
        record["phase"] = "implementation_completed"
        save_state(state_file, state)

    if record["phase"] == "implementation_completed":
        validate_changed_paths(changed_paths())
        record["phase"] = "audit_started"
        save_state(state_file, state)

    if record["phase"] in {"audit_started", "audit_failed"}:
        if current_head() != record["start_head"]:
            raise RunnerError(f"HEAD changed during {stage.stage_id}")
        print(f"\n=== {stage.stage_id}: independent audit ===")
        try:
            run_codex_phase(stage, "audit", stage_dir)
        except RunnerError:
            record["phase"] = "audit_failed"
            save_state(state_file, state)
            raise
        if current_head() != record["start_head"]:
            raise RunnerError(
                f"Codex changed git history during {stage.stage_id} audit"
            )
        record["phase"] = "audit_completed"
        save_state(state_file, state)

    if record["phase"] == "audit_completed":
        record["changed_paths"] = validate_stage_diff(stage)
        record["phase"] = "gate_started"
        save_state(state_file, state)

    if record["phase"] in {"gate_started", "gate_failed"}:
        print(f"\n=== {stage.stage_id}: external gate ===")
        try:
            run_gates(stage, stage_dir)
            record["changed_paths"] = validate_stage_diff(stage)
        except RunnerError:
            record["phase"] = "gate_failed"
            save_state(state_file, state)
            raise
        record["phase"] = "gate_passed"
        save_state(state_file, state)

    if record["phase"] in {"gate_passed", "commit_failed"}:
        print(f"\n=== {stage.stage_id}: commit ===")
        try:
            commit = commit_stage(stage)
        except RunnerError:
            record["phase"] = "commit_failed"
            save_state(state_file, state)
            raise
        record.update(phase="committed", commit=commit)
        save_state(state_file, state)
        print(f"Committed {stage.stage_id}: {commit}")


def archive_completed_state(state_file: Path, state: dict[str, Any]) -> None:
    state["completed_at"] = dt.datetime.now(dt.timezone.utc).isoformat()
    final_state = Path(state["run_dir"]) / "final-state.json"
    atomic_write_json(final_state, state)
    state_file.unlink()
    print(f"\nPlan selection completed. Evidence: {state['run_dir']}")


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Implement Lumi roadmap epics with two Codex exec calls each"
    )
    selection = parser.add_mutually_exclusive_group()
    selection.add_argument(
        "--from",
        dest="from_stage",
        metavar="EPIC",
        help="start at EPIC and continue through the configured roadmap",
    )
    selection.add_argument(
        "--only",
        dest="only_stage",
        metavar="EPIC",
        help="run exactly one epic",
    )
    selection.add_argument(
        "--release",
        metavar="VERSION",
        help=(
            "continue from the first uncommitted epic through the end of VERSION"
        ),
    )
    parser.add_argument(
        "--resume",
        action="store_true",
        help="resume the unfinished checkpoint",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="validate and print the selected epics without changing anything",
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="list all configured epics",
    )
    parser.add_argument(
        "--self-check",
        action="store_true",
        help="validate the hardcoded roadmap configuration",
    )
    parser.add_argument(
        "--state-root",
        type=Path,
        default=DEFAULT_STATE_ROOT,
        help=argparse.SUPPRESS,
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    try:
        validate_stage_definitions()
        if args.self_check:
            print(f"Configured epics: {len(STAGES)}")
            print("Plan runner configuration is valid.")
            return 0
        if args.list:
            for stage in STAGES:
                print_stage(stage)
            return 0
        if args.resume and (args.from_stage or args.only_stage or args.release):
            raise RunnerError(
                "--resume cannot be combined with --from, --only or --release"
            )

        selected = select_stages(args.from_stage, args.only_stage, args.release)
        if args.dry_run:
            print("Dry run; no Codex calls, gates or commits will run.\n")
            print_codex_configuration()
            print()
            if not selected:
                print(f"Release {args.release} is already complete.")
                return 0
            for stage in selected:
                print_stage(stage)
            return 0
        if not selected:
            print(f"Release {args.release} is already complete; nothing to run.")
            return 0

        print_codex_configuration()
        state_file = args.state_root / "state.json"
        if args.resume:
            if not state_file.is_file():
                raise RunnerError(f"No checkpoint to resume at {state_file}")
            state = load_state(state_file)
            selected = verify_resume_state(state)
            preflight(require_clean=False, require_auth=True)
        else:
            preflight(require_clean=True, require_auth=True)
            state_file, state = create_state(selected, args.state_root)

        while state["current_index"] < len(selected):
            stage = selected[state["current_index"]]
            run_stage(stage, state=state, state_file=state_file)
            state["current_index"] += 1
            save_state(state_file, state)
        archive_completed_state(state_file, state)
        return 0
    except RunnerError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("\nInterrupted. Resume later with --resume.", file=sys.stderr)
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
