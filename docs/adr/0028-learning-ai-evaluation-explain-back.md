# ADR 0028: AI-черновики и source-backed оценка learning

Status: `accepted`

Дата: 2026-07-26

## Контекст

Deterministic learning и scheduling уже используют revision-bound
`LearningSource`, immutable item/session snapshots и append-only attempts.
AI-расширение не должно создавать отдельный provider, очередь или
непроверяемый grading path.

## Решение

1. Генерация заданий и оценка открытого ответа являются обычными
   `AiTask` вида `generate_learning_items` и `evaluate_open_answer`. Они
   используют общий fenced Job runtime, OpenRouter BYOK, `AiContextPack`,
   retry/cancel и typed `AiArtifact`.
2. В registry добавлены `question-set-artifact.v1` и
   `open-answer-evaluation.v1`. Результат принимается только после строгой
   проверки bounds, answer shape, duplicates и citation ids.
3. Generated items атомарно импортируются как `ai_generated/draft`.
   Активация остаётся отдельным revision-checked действием пользователя.
   Связь item → task/artifact/citations хранится отдельно и не меняет
   immutable item revision.
4. AI evaluation не перезаписывает deterministic attempt. Каждая повторная
   оценка — новый immutable artifact/result, поэтому dispute/re-evaluate
   сохраняют историю.
5. `not_evaluated` не является нулевой оценкой: он не может содержать factual
   feedback или citations. При отсутствии provider пользователь сохраняет
   обычный self-check.
6. Explain-back использует `LearningSessionKind::ExplainBack`, те же
   evaluation tasks и последовательные immutable feedback results. Ответ
   пользователя передаётся как untrusted data; стиль и грамматика не являются
   критерием.

## Последствия

- capability `learning-ai` публикуется только вместе с готовыми provider,
  queue и explicit-context prerequisites;
- generated drafts не попадают в active tests до явной активации;
- provider failure остаётся retryable AI state и не создаёт false negative;
- voice/transcription остаются следующим эпиком и расширяют тот же evaluation
  contract.

## Отклонённые варианты

- Синхронный ad hoc вызов provider из learning route: дублирует retry,
  cancellation, provenance и rate-limit boundaries.
- Автоматическая активация generated items: снижает доверие и загрязняет
  scheduling.
- Хранение AI outcome прямо в mutable attempt: уничтожает историю повторной
  оценки и смешивает deterministic evidence с provider judgment.
