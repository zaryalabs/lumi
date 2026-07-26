# Единый план реализации Lumi 0.2.0–0.5.0

Status: `active`

Последнее обновление: 2026-07-26

## Назначение

Этот документ задаёт единую последовательность крупных продуктовых эпиков от
текущего состояния до завершения основного Web product roadmap `0.2.0–0.5.0`.
Он не заменяет подробные contracts, внутренние workstreams, acceptance criteria
и канонические продуктовые решения.

Roadmap заканчивается первым закрытым Web-контуром AI, learning, Desk/search и
Community Spaces. Это не означает завершение полного target design `Final v01`:
Knowledge Base, native full-copy replicas, Obsidian, plugin platform, будущие
источники, public Spaces, hosted AI и private/decentralized mode остаются
отдельными последующими направлениями.

## Модель исполнения

1. В работе находится один продуктовый эпик.
2. Внутри эпика backend, Web, tests, documentation и bounded spikes могут
   выполняться как независимые workstreams после фиксации общего contract.
3. Внутренние пакеты `A*`, `B*`, `C*`, gates и checklists из подробных планов
   являются картой работ, а не отдельными последовательными checkpoint runner.
4. Эпик закрывается только сквозным пользовательским outcome, миграциями,
   targeted tests, обязательным общим quality gate и документацией.
5. Незавершённая возможность остаётся выключенной capability flag.
6. Изменение scope сначала вносится в канонические документы и подробный план,
   затем отражается здесь.
7. После выпуска релиза его временный план архивируется или удаляется, а
   ROADMAP переводится на следующий незавершённый эпик.

## Сверка фактической готовности

Проверено по repository state и Git history на 2026-07-26.

Готово:

- baseline S1 Web Reader, описанный в корневом `README.md`;
- `0.2.0 / Stage 0`: spikes, ADR, threat review и release scope;
- `0.2.0 / Contract Freeze 1`: AI/MCP contracts, fixtures, mocks и router
  seams;
- `0.2.0 / A1`: PostgreSQL persistence, reusable encrypted `SecretStore`,
  общий fenced `JobRuntime`, import adapter, owner/idempotency invariants и
  transactional artifact publication;
- `0.2.0 / E1`: bounded explicit source context, OpenRouter BYOK, глобальный
  durable streaming chat, Reader selection handoff и source-backed citations;
- `0.2.0 / E2`: durable AI queue/internal worker, chapter/material summaries,
  Queue/Reader UX, retry/cancel/bulk и manual-edit candidate policy;
- `0.2.0 / E3`: revocable account-scoped MCP transport, product tools,
  fenced external worker и Web/MCP parity;
- `0.2.0 / E4`: generated abridged `.lum`, portable provenance, ordinary
  import validation, atomic library publication и переходы к оригиналу;
- `0.3.0 / E1`: revision-bound learning sources, durable completion и one-shot
  Reader offer, ручные versioned items, deterministic grading/self-check,
  immutable sessions, attempts, source jump и reload/resume;
- `0.3.0 / E2`: ordered hints/source evidence, versioned FSRS schedules,
  bounded Challenges `Сегодня`, daily limit, pause/resume/snooze/manual-only
  без штрафного backlog;
- release evidence A1 зафиксирован commit trailer
  `Lumi-Plan-Stage: 0.2.0/A1`.

Ещё не готово:

- AI/voice learning, общий learning release gate, Desk/search/RAG и Community
  Spaces.

Persistent server публикует capabilities `ai-provider-openrouter`,
`ai-provider-byok`, `ai-explicit-context`, `ai-global-chat`, `ai-task-queue`,
`ai-summary-artifacts`, `mcp-account-agent`, `mcp-ai-worker` и
`ai-abridged-lum`, а также route group `learning` и capability
`learning-core`.

После перехода на новую шкалу выполнено `6/18` продуктовых эпиков. Stage 0,
Contract Freeze 1 и A1 остаются закрытыми prerequisites.

Repository-side gates `make c`, PostgreSQL/compatibility/security/performance,
полный Web E2E и локальный staging image smoke проходят. Внешний staging
operator acceptance остаётся release gate окружения и не меняет статус
реализации продуктовых эпиков.

В прежней шкале это соответствовало `3/57` закрытым checkpoint roadmap, но
такой процент отражал в основном количество мелких шагов, а не продуктовую
готовность. Поэтому новая шкала начинается с нуля завершённых end-to-end
эпиков и отдельно сохраняет перечень уже готового foundation выше.

Текущий следующий эпик — `0.3.0/E3: AI-обучение и explain-back`.

## Последовательность

### 1. Lumi 0.2.0 — ИИ, саммари, чат и MCP

Подробный план:
[`0.2.0-ai-plan.md`](0.2.0-ai-plan.md).

1. [x] **E1 — Персональный AI-ассистент.** Explicit source context,
   BYOK/OpenRouter, settings, global chat, durable streaming conversations,
   citations и Reader selection handoff.
2. [x] **E2 — AI-задачи и саммари.** Durable queue/internal worker,
   chapter/material summaries, Reader actions, Queue UX, retry/cancel/bulk и
   manual-edit policy.
3. [x] **E3 — Внешние агенты через MCP.** Revocable connections, transport,
   account-scoped product tools, task claim/complete и Web/MCP parity.
4. [x] **E4 — Производные материалы и выпуск.** Abridged `.lum`, provenance,
   internal/MCP executor parity, atomic publication и полный release hardening.

Результат: реализован repository-side выпуск единого Web/MCP AI-контура с
общими task, artifact, context и authorization contracts; workspace и web
package имеют версию `0.2.0`. Внешнее staging acceptance остаётся отдельным
operator gate.

### 2. Lumi 0.3.0 — обучение после чтения

Начинается только после `0.2.0/E4`.

Подробный план:
[`0.3.0-learn-plan.md`](0.3.0-learn-plan.md).

1. [x] **E1 — Обучение после чтения.** Learning foundation, completion,
   deterministic grading, durable sessions/attempts, Reader offer, source jump
   и reload/resume.
2. [x] **E2 — Повторение и Challenges.** FSRS, hints/evidence, bounded
   `Сегодня`, pause/resume/snooze/manual-only и scheduling UX.
3. [ ] **E3 — AI-обучение и explain-back.** Generated drafts, open-answer
   evaluation, iterative cited feedback и no-provider/self-check fallback.
4. [ ] **E4 — Голосовой контур.** Generic audio attachment, browser recording,
   transcription, transcript review, grading/explain-back и retention/delete.
5. [ ] **E5 — Learning platform и выпуск.** MCP learning parity, общая
   интеграция, security/accessibility/performance, runbook и release acceptance.

Результат: deterministic learning, scheduling, AI explain-back и voice flow
закрыты отдельными пользовательскими вертикалями.

### 3. Lumi 0.4.0 — записи, Desk, поиск и RAG

Начинается только после `0.3.0/E5`.

Подробный план:
[`0.4.0-notes-and-desk-plan.md`](0.4.0-notes-and-desk-plan.md).

1. [ ] **E1 — Records v2 и Rich Reader.** Совместимая Annotation v2,
   targets, rich highlights/notes, Reader CRUD, migration и export.
2. [ ] **E2 — Голосовые записи и связи.** Voice Notes, audio lifecycle,
   stable `LinkTarget`, wikilinks, unresolved/ambiguous links и backlinks.
3. [ ] **E3 — Поисковое ядро.** BM25 + fastText, source-aware chunks,
   indexing/rebuild jobs, permission-aware query/retrieval API и benchmarks.
4. [ ] **E4 — Desk и единый поиск.** Desk projection/surfaces, global,
   library, Reader и Desk search, routing, inline edit и MCP parity.
5. [ ] **E5 — RAG по записям и выпуск.** Record-scoped retrieval в общем
   AI-чате, citations, сквозной Reader → Desk → Search → Chat flow и release
   hardening.

Результат: единая модель записей проходит через Reader, Desk, поиск, MCP и
record-scoped RAG без второго контура данных.

### 4. Lumi 0.5.0 — социальные пространства

Начинается только после `0.4.0/E5`.

Подробный план:
[`0.5.0-social-plan.md`](0.5.0-social-plan.md).

1. [ ] **E1 — Community Spaces и доступ.** Social foundation, Community/
   SyncSpace boundary, create/preview/join, roles, links, membership UX и
   permission/security gate.
2. [ ] **E2 — Публикация и сопоставление материалов.** Share flow,
   fingerprints, shared identity, claims, conservative matching и
   import-own-copy.
3. [ ] **E3 — Совместное чтение.** Shared anchors, discussions, explicit
   published highlights, Reader social layer, moderation и unresolved states.
4. [ ] **E4 — Коммуникации и выпуск.** Space chat, activity, social search
   events, MCP parity, polling, backup/restore и multi-account release
   acceptance.

Результат: два аккаунта проходят полный закрытый Community Space flow без
раскрытия source-файлов и личных записей.

## Финальная точка текущего roadmap

Roadmap `0.2.0–0.5.0` завершён, когда закрыт `0.5.0/E4`, критерии завершения
всех четырёх планов выполнены, `make c` и обязательные Web E2E проходят, а
долгоживущие решения перенесены из `docs/tmp-plans/` в канонические документы.

Последующие функции планируются отдельной серией релизов и не считаются
скрытыми условиями завершения `0.5.0`.
