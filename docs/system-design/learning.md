# Механики обучения

Status: draft

## Контекст

Learning-подсистема нужна, чтобы чтение не заканчивалось последней страницей.
Она превращает материалы, заметки и ИИ-артефакты в упражнения: вопросы,
тесты, флешкарты, повторение и объяснение своими словами с обратной связью.

Vision задает несколько ключевых сценариев:

- тест по пройденному материалу после завершения чтения;
- ответы текстом или голосом;
- вопросы по ранее прочитанному с подсказками;
- повторение с учетом кривой забывания;
- упражнение, где пользователь объясняет прочитанное, а ИИ корректирует до
  понимания.

Learning не должен быть отдельным учебным приложением рядом с reader. Он должен
работать поверх тех же материалов, anchors, заметок, поиска и ИИ-задач.

## Пользовательские сценарии

- Пользователь заканчивает главу и получает короткий тест по прочитанному.
- Пользователь отвечает на вопрос текстом или голосом.
- Пользователь пропускает упражнение, чтобы не ломать поток чтения.
- Пользователь включает повторение и получает карточки в нужные дни.
- Пользователь просит "сделай карточки по этой главе"; Lumi ставит AI task и
  позже показывает результат.
- Пользователь запускает режим "объяснить своими словами": пишет или говорит
  объяснение, а система указывает пробелы, пока ответ не станет достаточно
  точным.
- Автор `lum` добавляет готовые exercises/flashcards в материал.
- Внешний агент создает questions/cards по очереди задач, если в Lumi нет
  API-ключа.

## Функциональные требования

### Типы упражнений

Базовые exercise families:

- `quiz` - single choice, multiple choice, true/false.
- `open_question` - открытый текстовый ответ.
- `flashcard` - front/back card.
- `cloze` - пропущенные слова/понятия.
- `hinted_question` - вопрос с раскрывающимися подсказками.
- `explain_back` - пользователь объясняет своими словами, ИИ проверяет и
  задает уточнения.
- `reflection_prompt` - неоцениваемый вопрос для заметки/осмысления.

Каждый item должен быть связан с source:

- material;
- document revision;
- chapter/section;
- anchor или page range;
- KB note;
- AI artifact provenance.

### Источники learning items

Learning item может появиться из:

- `lum` interactive block;
- user-created card/question;
- AI-generated task;
- imported Anki-like/Markdown data later;
- plugin provider;
- repeated highlight/note converted to card.

Generated items start as drafts. Пользователь может принять, отредактировать,
архивировать или regenerate.

### Генерация через ИИ

Reader/KB creates `AiTask`:

```text
generate_learning_items(material/chapter/anchor, item_types, difficulty)
```

AI result returns structured draft:

- questions;
- options/answers;
- explanations;
- source anchors;
- confidence/quality notes;
- suggested schedule hints.

Learning layer validates structure and creates `LearningItem` drafts.

Если API-ключа нет, task остается в очереди и может быть обработан внешним
агентом. Агент создает artifacts, которые Lumi импортирует как generated drafts.

### Explain-back mechanic

Explain-back - отдельный interactive AI scenario:

1. Lumi выбирает source scope: chapter/block/material.
2. Пользователь объясняет своими словами text или voice.
3. AI compares explanation against source context и expected concepts.
4. AI returns:
   - что верно;
   - что пропущено;
   - что искажено;
   - уточняющий вопрос или next prompt.
5. Loop continues until success criteria или user stops.
6. Итог сохраняется как attempt, feedback и optional KB note.

Ограничение: внутри Lumi этот mode requires direct AI availability через
user key/subscription, потому что он интерактивный и чувствителен к latency.
External agent integration can support the same mechanic only if agent owns the
UI и возвращает final artifacts/attempt summary back to Lumi.

### Voice answers

- Reader/learning UI can record audio answer.
- Audio is stored as voice note/learning attempt attachment.
- Transcription is AI task.
- Until transcript is available, attempt state is `pending_transcription`.
- Explain-back over voice requires transcription or multimodal provider.

### Scheduling

Learning schedule should support:

- due date;
- review state;
- answer quality;
- item difficulty;
- per-user settings;
- opt-out per material/folder.

Draft decision:

- Use FSRS-like or SM-2-like scheduler abstraction, not hardcoded UI logic.
- Store enough fields to replace algorithm later.
- Keep algorithm version in schedule records.

User can disable spaced repetition entirely or per source.

### Attempts and mastery

Each attempt records:

- item id;
- user answer or selected options;
- correctness/score;
- hints used;
- time spent;
- source context;
- feedback;
- created_at;
- client/device;
- algorithm update payload.

Mastery is derived from attempts and schedule, not manually edited primary
state. UI can show weak/strong concepts, missed questions and due items.

### Challenges screen

Vision mentions "Челенджи". This surface should include:

- due reviews;
- tests after completed chapters/materials;
- missed/skipped reading exercises;
- explain-back sessions;
- progress by material/concept;
- generated drafts waiting for approval.

## Нефункциональные требования

- **Optionality.** Learning should support reading flow, not block it.
- **Source-backed.** Every generated question should have source citation or
  anchor where possible.
- **Editable.** AI-generated items must be user-editable.
- **Offline-first where possible.** Existing items, attempts and schedules work
  offline. New AI generation may wait for provider/agent.
- **Explainability.** User should see why an answer is wrong and where to
  reread.
- **Privacy.** Learning attempts are private unless explicitly shared.
- **Durability.** Attempts and schedule changes sync reliably and do not depend
  on transient UI state.

## Модель данных

```text
LearningSource
  -> LearningItem[]
  -> LearningAttempt[]
  -> LearningSchedule
  -> MasteryState
```

Основные сущности:

- `LearningSource` - material/chapter/anchor/KB note that items are based on.
- `LearningItem` - question/card/exercise.
- `LearningItemRevision` - editable text/options/answer revision.
- `LearningAttempt` - user interaction with an item.
- `LearningSchedule` - due/repetition state.
- `LearningSession` - grouped challenge/test/explain-back session.
- `LearningHint` - hints attached to item.
- `LearningFeedback` - AI/manual feedback.
- `MasteryState` - derived per concept/source status.
- `LearningImportIssue` - invalid generated/imported item.

Learning item:

```text
LearningItem {
  id
  source_ref
  kind
  prompt
  answer_spec
  hints
  explanation
  difficulty
  status: draft | active | archived | rejected
  generated_by_task_id
  created_at
  updated_at
}
```

Attempt:

```text
LearningAttempt {
  id
  item_id
  session_id
  answer_payload
  score
  correctness
  hints_used
  feedback
  started_at
  finished_at
}
```

## Реализация

### Item generation pipeline

1. User or system selects source scope.
2. Search/retrieval gathers source chunks and citations.
3. AI task is created with structured output schema.
4. Provider/agent returns draft items.
5. Validator checks schema, source refs, duplicate questions, empty answers.
6. Drafts appear in review queue or auto-activate if policy allows.
7. Search/KB indexes accepted items.

### Embedded `lum` exercises

`lum` blocks like `lum:quiz` and `lum:flashcard` compile to `LearningItem`
templates. User attempts still live outside source package and sync as personal
data.

### Explain-back pipeline

Inside Lumi:

1. Create interactive `AiConversation` with mode `explain_back`.
2. Retrieve source context.
3. Send user answer and rubric/context to provider.
4. Store each turn as attempt event.
5. On completion, write `LearningAttempt` and optional artifact summary.

External agent path:

1. Lumi exports task with source scope and desired mechanic.
2. Agent runs its own UI/conversation.
3. Agent returns summary, score, missing concepts and optional KB note.
4. Lumi stores final artifact/attempt, not the full interactive UI state unless
   agent provides it.

### Scheduling abstraction

Define scheduler trait/service:

```text
review(item, previous_schedule, attempt) -> next_schedule
```

Stored schedule includes algorithm name/version and payload. This keeps room for
FSRS/SM-2 experimentation without migrating all attempts immediately.

## Интеграции и зависимости

- **Reader.** Reader shows inline exercises and records reading events.
- **Синхронизация.** Items, attempts and schedules are personal sync objects.
- **Поиск.** Retrieval supplies source context; learning artifacts are indexed.
- **База знаний.** Cards/questions can link to KB notes and concepts.
- **ИИ.** AI generates items, evaluates open answers and powers explain-back.
- **Social.** Shared folders can later share challenge templates/results, but
  personal attempts remain private by default.
- **Плагины.** Plugins can add exercise types, import/export formats and
  scheduler algorithms with explicit capabilities.

## Альтернативы

- `rejected`: make every chapter quiz mandatory. This harms reading flow.
- `rejected`: store AI-generated questions as final without review/provenance.
  Bad generated items damage trust and search quality.
- `rejected`: implement explain-back as non-interactive queued artifact only.
  The core value is iterative correction.
- `revisit`: FSRS as accepted default algorithm. Likely good, but needs
  evaluation and implementation details.
- `revisit`: Anki export/import. Useful for power users, but not core to
  first design pass.

## Открытые вопросы

- Which scheduler should be default: FSRS, SM-2, or simpler custom algorithm?
- What quality gates should generated questions pass before auto-activation?
- How much reading timeline should affect due dates and mastery?
- Should learning results ever be shareable in shared folders, and at what
  privacy granularity?
- What voice transcription provider path is acceptable for offline/mobile?
