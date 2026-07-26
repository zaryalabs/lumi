# Механики обучения

Status: accepted

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

## Продуктовые принципы

- **Чтение важнее проверки.** Завершение главы или материала фиксируется до
  предложения упражнений. Отказ, пропуск или ошибка learning-сессии не меняют
  прогресс чтения.
- **Одна сессия, независимые механики.** Сразу после чтения Lumi может
  предложить быструю проверку, explain-back и будущие повторения, но пользователь
  выбирает любую комбинацию, а не проходит обязательную линейную воронку.
- **Сначала вспоминание, затем подсказка.** Ответ не показывается до первой
  попытки или явного действия пользователя. Подсказки раскрываются постепенно и
  учитываются при оценке попытки.
- **Оценка помогает перечитать, а не выносит вердикт.** Feedback объясняет
  пробел, ссылается на source anchors и предлагает следующее действие. Lumi не
  делает выводов об интеллекте или способностях пользователя.
- **Без ИИ остается полезный режим.** Готовые тесты, карточки, подсказки,
  расписание и самопроверка работают без provider. Генерация, автоматическая
  оценка открытого ответа и interactive explain-back требуют доступного ИИ.
- **У пользователя остается контроль.** Автогенерация, голос, расписание и
  напоминания являются разными настройками и включаются независимо.

## Пользовательские сценарии

- Пользователь заканчивает главу и получает необязательное предложение
  проверить себя за несколько минут, продолжить чтение или закрыть reader.
- Пользователь проходит короткий тест с вариантами либо отвечает на открытый
  вопрос текстом или голосом.
- Пользователь пропускает упражнение, чтобы не ломать поток чтения.
- Пользователь включает повторение и получает карточки в нужные дни.
- Пользователь раскрывает подсказки по одной, а после ответа переходит к
  исходному фрагменту и возвращается в ту же learning-сессию.
- Пользователь ставит повторения на паузу глобально или для материала, не теряя
  созданные items и историю попыток.
- Пользователь просит "сделай карточки по этой главе"; Lumi ставит AI task и
  позже показывает результат.
- Пользователь запускает режим "объяснить своими словами": пишет или говорит
  объяснение, а система указывает пробелы, пока ответ не станет достаточно
  точным.
- Автор `lum` добавляет готовые exercises/flashcards в материал.
- Внешний агент создает questions/cards по очереди задач, если в Lumi нет
  API-ключа.

## Функциональные требования

### Сессия после чтения

Завершение главы, раздела или всего материала создает один idempotent
`completion event`. Если для этого scope есть активные learning items, reader
показывает компактное предложение:

- `Проверить себя` - короткая immediate-recall сессия;
- `Объяснить своими словами` - отдельный explain-back flow;
- `Напомнить позже` - включить или подтвердить повторения для source;
- `Не сейчас` - закрыть предложение без потери прогресса;
- `Не предлагать для этого материала` - отключить автоматические предложения
  для source, оставив ручной запуск доступным.

Предложение не должно перекрывать последнюю страницу до завершения чтения и не
должно появляться повторно при каждом открытии уже завершенной главы. Повторно
запустить любую механику можно из карточки материала, reader и экрана
`Челленджи`.

Immediate-recall сессия по умолчанию:

- занимает ориентировочно 3-5 минут;
- содержит 3-7 заданий, если items достаточно;
- покрывает разные ключевые понятия source, а не несколько формулировок одного
  факта;
- смешивает закрытые и открытые задания, когда оба типа доступны;
- показывает итог только после ответов и дает переходы к источнику;
- не создает автоматически расписание повторений без согласия пользователя.

Если готовых items нет, Lumi честно предлагает создать их через ИИ или начать
explain-back. Завершение чтения не должно ждать генерацию. Автогенерация до
завершения source допустима только при явно включенной пользователем политике и
с соблюдением AI context/privacy policy.

#### Реализованный baseline `0.3.0/E1`

Web Reader фиксирует progress до `ReadingCompletion`, после чего один раз
показывает offer для immutable material/content-unit scope. Offer можно закрыть,
отложить или отключить для материала; ручной вход через карточку материала
остаётся доступен.

Ручные active items формируют immediate-recall session с immutable ordered
snapshot. Закрытые задания оцениваются детерминированно, открытые используют
явную self-check. Session и attempts читаются по стабильному hash route, поэтому
reload не теряет ответов. `Открыть источник` записывает evidence event,
передаёт revision-bound target в Reader и сохраняет route возврата.

Точные persistence/API решения закреплены в
[`ADR 0026`](../adr/0026-learning-completion-items-sessions.md). Scheduling,
AI generation/evaluation, explain-back и voice не объявляются capability
`learning-core` и поставляются следующими эпиками.

#### Реализованный scheduling vertical `0.3.0/E2`

Ordered hints и открытие source сохраняются как append-only evidence и входят в
attempt. Явный review rating обновляет versioned FSRS schedule атомарно с
attempt; корректность и assistance дают только консервативную рекомендацию и не
подменяют пользовательский rating.

`#challenges` показывает bounded `Сегодня`, unscheduled `Закрепить сейчас`,
due/ready/draft counts и отдельные состояния disabled/manual-only/done.
Account daily limit по умолчанию равен `20`. Global disable и manual-only
сохраняют schedules; source pause исключает material из overdue и reminder
projection, а resume не раскрывает весь накопленный backlog. Snooze переносит
только unanswered items без fake failures.

Persistent server публикует capability `learning-scheduling`. Алгоритм,
параметры, mapping и upgrade contract закреплены в
[`ADR 0027`](../adr/0027-fsrs-scheduling-challenges.md).

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

Для бесшовной post-reading сессии пользователь может отдельно разрешить
автоактивацию личных AI-generated items после структурной и source-grounding
валидации. По умолчанию generated items требуют просмотра; встроенные автором
упражнения и вручную созданные пользователем items могут быть активны сразу.

### Ответы и проверка теста

- Для `single choice`, `multiple choice` и `true/false` authoritative answer
  хранится в `answer_spec`; результат вычисляется детерминированно без ИИ.
- Для открытого вопроса пользователь выбирает ввод текстом или голосом. После
  transcription он может исправить распознанный текст до отправки на оценку.
- Голосовой ответ на закрытый тест является accessibility shortcut: Lumi
  распознает номер или текст варианта и просит подтвердить неоднозначный выбор.
- Открытый ответ проверяется по rubric и expected concepts, а не по совпадению
  строки с эталоном.
- Пользователь всегда может выбрать `Показать ответ` или `Оценить себя`. Такая
  попытка помечается как self-checked и не выдается за автоматически
  проверенную.
- После ответа UI показывает объяснение, использованные подсказки и source
  anchors. Ошибка ИИ или transcription не должна превращаться в неправильный
  ответ без возможности исправления.

### Вопросы с подсказками

`hinted_question` поддерживает упорядоченные уровни помощи:

1. направление мысли или категория ответа;
2. ключевое понятие, контекст или исключение;
3. близкий к ответу фрагмент либо переход к source anchor;
4. полный ответ с объяснением.

Item может иметь меньше уровней, но порядок фиксируется в его revision.
Раскрытая подсказка записывается в attempt. Верный ответ после подсказки
остается полезным, однако снижает evidence самостоятельного вспоминания и
влияет на следующее расписание. Пользователь может открыть источник в
отдельном слое и затем вернуться к тому же вопросу; такое действие также
записывается как помощь, а не как «чистое» вспоминание.

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

В release обучения generation ограничена явно выбранным material/chapter/anchor
scope и использует общий `SourceContextResolver`. Полный search index и
library-wide retrieval для этого не требуются.

### Explain-back mechanic

Explain-back - отдельный interactive AI scenario:

1. Lumi выбирает source scope: chapter/block/material.
2. Перед началом Lumi показывает scope и короткую инструкцию объяснить материал
   так, как пользователь объяснял бы его другому человеку.
3. Пользователь объясняет своими словами text или voice.
4. Для voice пользователь проверяет transcript до оценивания.
5. AI compares explanation against source context, expected concepts и rubric.
6. Каждая итерация оценивает отдельно:
   - фактическую корректность;
   - покрытие ключевых понятий;
   - связи, причинность и ограничения, если они существенны для source;
   - ясность, достаточную для понимания, но не стиль речи или грамотность сами
     по себе.
7. AI returns:
   - что верно;
   - что пропущено;
   - что искажено;
   - source citations для утверждений feedback;
   - один наиболее полезный уточняющий вопрос или next prompt.
8. Loop continues until success criteria, user chooses to finish или исчерпан
   настроенный session limit.
9. Итог сохраняется как attempt, structured feedback и optional KB note.

Итоговые состояния не должны имитировать точную экзаменационную оценку:

- `understood` - ключевые понятия раскрыты без существенных ошибок;
- `partial` - основа верна, но есть заметные пробелы;
- `needs_review` - есть существенное искажение или не раскрыта основная идея;
- `not_evaluated` - пользователь завершил раньше, ИИ недоступен или feedback
  нельзя надежно привязать к источнику.

ИИ не должен снижать результат только за другую формулировку, акцент,
неидеальную речь или отсутствие несущественных деталей. Если source context
недостаточен или противоречив, результат становится `not_evaluated`, а не
догадкой модели. Пользователь может оспорить feedback, открыть цитату,
перезапустить оценку или сохранить попытку без оценки.

Ограничение: внутри Lumi этот mode requires direct AI availability через
user key/subscription, потому что он интерактивный и чувствителен к latency.
External agent integration can support the same mechanic only if agent owns the
UI и возвращает final artifacts/attempt summary back to Lumi.

### Voice answers

- Reader/learning UI can record audio answer.
- Audio сохраняется как общий `AudioAttachment`, которым владеет personal
  scope; learning attempt хранит только stable attachment reference.
- Transcription is durable AI task. Встроенный server-side worker использует
  OpenAI Audio Transcriptions API с моделью `whisper-1` и отдельным
  account-scoped OpenAI API credential согласно
  [ADR 0025](../adr/0025-openai-whisper-transcription.md).
- Until transcript is available, attempt state is `pending_transcription`.
- Transcription produces a versioned `TranscriptArtifact`. Original transcript
  и accepted edited transcript сохраняют provenance как разные revisions,
  а не встраиваются в learning attempt или voice-note payload.
- Retention of original audio follows a separate privacy setting. User can
  delete audio after transcription while keeping the accepted transcript.
- Explain-back over voice requires transcription or multimodal provider.

Upload/download authorization, MIME/size limits, checksum, retention и
refcount/GC принадлежат общему audio attachment contract. Learning не создает
отдельный uploader или blob lifecycle.

### Scheduling

Learning schedule should support:

- due date;
- review state;
- answer quality;
- item difficulty;
- per-user settings;
- opt-out per material/folder.

Decision:

- Use FSRS as the default scheduler behind a `Scheduler` port.
- Store enough fields to replace algorithm later.
- Keep algorithm version in schedule records.
- Initial adapter is reproducible FSRS 4.5, desired retention `0.9`, version
  `fsrs-4.5-lumi-v1`; a newer FSRS generation is an explicit replayed upgrade,
  not an in-place semantic change.

FSRS models estimated retention from actual attempts; UI may call this
`повторение с учетом забывания`, but should not promise a universal fixed
forgetting curve. A first exposure is not treated as a successful review until
the user attempts recall.

Controls:

- enable/disable scheduling globally;
- enable/disable or pause it for a material/folder;
- snooze a session without grading all due items as failed;
- choose a lightweight daily limit or review only on manual launch;
- keep reminders off while scheduling remains enabled;
- resume from stored schedule or restart a source schedule explicitly.

Disabling or pausing repetition never deletes items, attempts or schedule
history. Paused items do not become an ever-growing overdue counter and do not
send reminders. They remain available for manual practice. On resume, scheduler
recomputes the next actionable session from persisted state rather than
presenting the entire accumulated queue at once.

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

The default view prioritizes a bounded `Сегодня` session, not an unbounded
backlog. It distinguishes:

- `Закрепить сейчас` - immediate tests for recently completed scopes;
- `Повторить` - due scheduled items;
- `Объяснить` - optional explain-back prompts;
- `Черновики` - generated items that still need review.

User can filter by material and dismiss, snooze or pause a source directly from
this screen. Streaks, leagues and punitive overdue counters are outside the
baseline: the surface optimizes understanding and return to source, not daily
engagement at any cost.

## Нефункциональные требования

- **Optionality.** Learning should support reading flow, not block it.
- **Source-backed.** Every generated question should have source citation or
  anchor where possible.
- **Editable.** AI-generated items must be user-editable.
- **Offline-first where possible.** Existing items, attempts and schedules work
  offline. New AI generation may wait for provider/agent.
- **Explainability.** User should see why an answer is wrong and where to
  reread.
- **Boundedness.** A learning session has a visible estimated size and can be
  stopped without marking unanswered items wrong.
- **Accessibility.** Voice is an alternative input, not a separate lower- or
  higher-value exercise path.
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
- `LearningRubric` - expected concepts and evaluation criteria for open answer.
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
  rubric
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
  source_opened
  evaluation_state
  feedback
  started_at
  finished_at
}
```

Session:

```text
LearningSession {
  id
  source_ref
  kind: immediate_recall | scheduled_review | explain_back | manual_practice
  trigger: completion | due | manual
  state: offered | in_progress | completed | dismissed | abandoned
  item_ids
  estimated_minutes
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
future scheduler experimentation without migrating all attempts immediately.
FSRS is the accepted default for the target design; SM-2 or simpler algorithms
can exist as alternative plugins/adapters if they prove useful.

## Интеграции и зависимости

- **Reader.** Reader shows inline exercises and records reading events.
- **Desk.** Показывает persistent learning items, attempts,
  due/missed/skipped/completed states и material-level mastery summary.
  Активная challenge/review session остается в learning surface.
- **Синхронизация.** Items, attempts and schedules are personal sync objects.
- **Поиск.** Retrieval supplies source context; learning artifacts are indexed.
- **База знаний.** Cards/questions can link to KB notes and concepts.
- **ИИ.** AI generates items, evaluates open answers and powers explain-back.
- **MCP.** Account-scoped tools list learning items, create supported
  generation tasks and submit answers through the same application services,
  permissions and idempotency rules as Web.
- **Social.** Community Spaces can later share challenge templates/results, but
  personal attempts remain private by default.
- **Плагины.** Plugins can add exercise types, import/export formats and
  scheduler algorithms with explicit capabilities.

## Альтернативы

- `rejected`: make every chapter quiz mandatory. This harms reading flow.
- `rejected`: store AI-generated questions as final without review/provenance.
  Bad generated items damage trust and search quality.
- `rejected`: implement explain-back as non-interactive queued artifact only.
  The core value is iterative correction.
- `rejected`: treat skipped or unanswered post-reading exercises as failures.
  Learning remains optional and must not rewrite reading completion.
- `rejected`: show every paused review as overdue after resume. This turns an
  opt-out into punishment and creates an unusable backlog.
- `accepted`: FSRS as default scheduler through a replaceable `Scheduler` port.
- `revisit`: Anki export/import. Useful for power users, but not core to
  first design pass.

## Открытые вопросы

- What quality gates should generated questions pass before auto-activation?
- How much reading timeline should affect due dates and mastery?
- What default daily limit should the first Challenges UI offer after usability
  testing?
- Should a generated rubric be shown before an explain-back attempt, only after
  it, or behind an explicit action?
- Should learning results ever be shareable in Community Spaces, and at what
  privacy granularity?
- Нужен ли будущему offline/mobile profile локальный Whisper runtime или
  достаточно отложенной server-side транскрибации через OpenAI?
