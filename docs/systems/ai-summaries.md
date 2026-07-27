# ИИ-саммари и сокращенные материалы

Status: accepted

## Контекст

Саммари в Lumi закрывает три разных сценария:

- быстро пересказать небольшой фрагмент в глобальном чате;
- сохранить саммари главы или целого материала для повторного обращения;
- создать сокращенную версию материала, которую можно читать как отдельную
  книгу.

Эти сценарии используют общий AI provider и source-backed context, но дают
разные продуктовые результаты. Ответ в чате не обязан становиться artifact, а
сокращенная книга не должна храниться как длинное сообщение или притворяться
новой импортированной revision оригинала.

Документ фиксирует рамочный продуктовый контракт. Точные schemas, prompts,
generation pipeline и UI states будут детализированы в implementation plan.

## Решения

- Саммари выделения, абзаца или небольшого раздела создается в глобальном чате.
- Сохраненное саммари главы запускается из reader кнопкой в конце главы, а не
  через chat flow.
- Сохраненное саммари целого материала запускается как material action.
- Для одного source scope существует одно активное сохраненное саммари:
  отдельное для каждой главы и отдельное для всего материала.
- `brief` и `outline` являются формами одного `SummaryArtifact`, а не
  независимыми именованными документами.
- Новая генерация создает следующую внутреннюю версию результата и после
  принятия заменяет активную; пользователь не управляет коллекцией параллельных
  вариантов саммари.
- Сокращенная версия входит в AI summary scope как отдельный крупный сценарий.
- Сокращенная версия создается как отдельный производный `Material` в формате
  `.lum` со связью с оригинальным material/revision и source anchors.

## Быстрое саммари в чате

Пользователь выделяет текст или прикрепляет небольшой раздел к глобальному
чату, после чего выбирает «Кратко перескажи» либо пишет собственную инструкцию.

Результат:

- является обычным assistant message;
- использует переданный selection context;
- по возможности содержит ссылки на source anchors;
- не создает `SummaryArtifact` автоматически;
- может быть сохранен отдельно будущим явным действием пользователя.

Этот путь оптимизирован для быстрого вопроса во время чтения и не использует
`AiTask`, фоновые progress UI или отдельный summary screen. Если chat flow
недоступен или результат не подходит, пользователь отдельно запускает
сохраненное chapter/material summary.

## Сохраненное саммари

### Scope

Поддерживаются два source scope:

- `chapter` — конкретная глава или структурный раздел;
- `material` — весь материал.

Саммари выбранного произвольного фрагмента пока остается только в чате.

### Формы

- `brief` — короткое содержание в нескольких основных тезисах;
- `outline` — структурированный конспект с заголовками и логическими разделами.

Форма является параметром активного `SummaryArtifact`. Если пользователь
меняет форму или запускает generation заново, Lumi создает новую версию того же
summary slot. После принятия она supersedes предыдущий результат.

Специализированные инструкции вроде «основные аргументы», «термины»,
«практические выводы» или «сюжет и персонажи» могут позже стать generation
options. На текущем этапе они не создают отдельные artifact kinds.

### Reader UX

В конце каждой главы reader показывает контекстное действие:

- `Создать саммари`, если результата еще нет;
- состояние генерации и progress, пока выполняется задача;
- `Открыть саммари`, когда результат готов;
- retry, если generation завершилась ошибкой.

Открытое саммари позволяет:

- прочитать результат;
- перейти по citation к исходному месту;
- изменить форму и перегенерировать;
- отредактировать текст;
- удалить результат.

Эта механика не открывает глобальный чат и не добавляет служебный разговор.

Material-level действие доступно из карточки/деталей материала и в конце
чтения. Точное расположение будет выбрано при UX-проработке.

### Выполнение

Сохраненное саммари создается через durable `AiTask`:

```text
summary action
  -> AiTask
  -> source context/chunks
  -> provider or external agent
  -> validation
  -> SummaryArtifact candidate
  -> active summary
```

Задача переживает reload, показывает progress, допускает cancellation/retry и
может быть выполнена direct provider или внешним агентом через AI task queue.
Пользователь может оставить ее в очереди либо выбрать `Выполнить сейчас`.
Подробный queue UX описан в
[`ai-task-queue.md`](ai-task-queue.md).

Для больших глав и материалов используется иерархическая обработка:

```text
source sections
  -> section summaries
  -> synthesis
  -> coverage/citation check
  -> final SummaryArtifact
```

Промежуточные результаты являются implementation data и не создают отдельные
пользовательские саммари.

## Один активный результат

Lumi не создает коллекцию вариантов «для новичка», «к экзамену» и подобных
именованных саммари. Рамочная уникальность:

```text
owner + source_revision + scope_kind + scope_ref -> active SummaryArtifact
```

История generation нужна для безопасной перегенерации и восстановления
предыдущего результата, но основной UI показывает одно активное саммари на
scope.

Если пользователь отредактировал результат, новая generation не перезаписывает
его молча. Сначала создается candidate, который пользователь может принять или
отклонить. Точная политика сравнения с ручными правками будет определена в
implementation plan.

## Сокращенный `.lum`-материал

### Продуктовая модель

Сокращенная версия — полноценный производный материал:

- появляется в библиотеке как отдельный `Material`;
- открывается через обычный reader;
- имеет собственные progress, highlights, notes и search surface;
- сохраняет структуру и порядок чтения оригинала настолько, насколько это
  полезно;
- позволяет перейти от сокращенного раздела к исходной главе или anchor;
- явно помечена как AI-generated abridged version.

Она не является `DocumentRevision` оригинального material. Производный
материал имеет собственные revisions, поэтому повторная генерация сокращенной
версии может публиковаться как новая revision именно производного материала.

```text
Original Material / DocumentRevision
  -> abridgement AiTask
  -> generated .lum package
  -> Derived Material / DocumentRevision
  -> Normalized Content Package
  -> ReadingDocument
```

### Формат

Результат создается в принятом формате `.lum`:

- строгий `lum.toml`;
- Markdown-файлы сокращенных глав;
- `spine`, сохраняющий маршрут чтения;
- title/language/source metadata;
- необходимые локальные resources;
- provenance original material/revision и source anchors.

Generated package проходит тот же import/validation pipeline, что обычный
`.lum`, и после успешной публикации может быть экспортирован пользователем.
Точная форма generated provenance в manifest или связанных Lumi metadata будет
определена без нарушения текущего `.lum` format contract.

### Генерация

Сокращение является отдельным крупным durable workflow внутри текущего scope:

1. Разбить оригинал по главам и source sections.
2. Построить сокращенную версию каждой главы с source refs.
3. Проверить покрытие основных идей, терминов и переходов.
4. Удалить межглавные повторы и согласовать структуру.
5. Собрать `.lum` source project/package.
6. Провалидировать package и импортировать его как derived material.
7. Опубликовать material только после успешной сборки.

Пользователь видит общий progress и может повторить failed workflow. Частично
собранный `.lum` не появляется в библиотеке как готовый материал.

## Source changes

`SummaryArtifact` и сокращенный material привязаны к конкретной
`DocumentRevision` оригинала. После появления новой source revision они
получают состояние «Источник изменился», но не обновляются автоматически.

Пользователь может:

- оставить существующий результат;
- перегенерировать summary;
- собрать новую revision сокращенного `.lum`-материала.

## Реализованный срез `0.2.0/E2–E4`

Сохранённые `brief`/`outline` саммари главы и материала реализованы поверх
общей durable queue и frozen `summary-artifact.v1`. Первый AI-authored
результат становится active автоматически. Ручная правка создаёт новую
immutable user-authored artifact revision и supersedes прежнюю. Последующая
генерация остаётся candidate до явного принятия или отклонения, поэтому
пользовательский текст не перезаписывается молча.

Reader показывает chapter action в конце структурной единицы; Library и
Reader дают material action, PDF — material и page-as-chapter scope. Summary
dialog показывает active/candidate/manual-edit/source-changed состояния,
cancel/retry и source navigation. История задач доступна на отдельной Queue
page.

Сокращение создаётся material-scoped задачей с профилем `brief` или
`balanced`. Внутренний provider и внешний MCP executor возвращают один и тот
же bounded `abridgement-artifact.v1`: заголовок, главы Markdown и точные
`SourceCitation`. Lumi не принимает готовый ZIP через AI boundary. Сервер
самостоятельно собирает portable `.lum`, добавляет
`META-INF/lumi/provenance.json`, затем прогоняет байты через обычный `.lum`
importer. Preflight выполняется до завершения задачи, поэтому payload с
неполными citations или невалидный package не становится успешным artifact.

Публикация library projection использует отдельную транзакцию после атомарного
завершения task/run/artifact. Crash между этими границами безопасен: startup
recovery повторно находит валидный candidate artifact и идемпотентно завершает
публикацию. Material, revision, normalized package, import job, authoritative
`material_derivations` и sync change становятся видимыми вместе. Исходная
revision не изменяется. Исходные package bytes сохраняются как source blob,
поэтому download производного material экспортирует тот же валидный `.lum`
вместе с portable provenance.

Library помечает сокращение как производный материал. Details показывает
immutable source revision, состояние «оригинал обновлён» и переходы по
сохранённым anchors; сам материал читается обычным Reader.

## Данные

Рамочная модель:

```text
SummaryArtifact {
  id
  source_material_id
  source_revision_id
  scope_kind: chapter | material
  scope_ref
  form: brief | outline
  content
  source_refs
  status
  generation_info
  created_at
  updated_at
}
```

Для сокращенного материала authoritative derived relationship хранится в
`material_derivations` и связывает новый `Material`/`DocumentRevision` с
source `Material`/`DocumentRevision`, task, artifact и полным набором source
refs. Web projection вычисляет `source_changed` относительно текущей active
revision оригинала. Portable копия тех же связей хранится внутри `.lum`;
обычный importer проверяет schema marker, checksums глав и citation mapping.

## Нефункциональные требования

- **Source-backed.** Сохраненные результаты содержат source refs и citations.
- **No silent overwrite.** Generation не уничтожает пользовательские правки.
- **Durability.** Chapter/material summary и abridgement выполняются через
  общий Job engine.
- **Validation.** `.lum` публикуется только после package/import validation.
- **Portability.** Сокращенный `.lum` можно экспортировать как пользовательский
  материал.
- **Replaceability.** Provider и внешний агент создают одинаковые artifacts и
  package inputs.

## Интеграции

- **Глобальный чат.** Быстрое саммари selection остается chat response.
- **Reader.** Показывает chapter summary actions в конце главы и открывает
  source citations.
- **AI core/jobs.** Выполняет summary и abridgement workflows.
- **Normalized content.** Дает source sections, anchors и принимает generated
  `.lum` через обычный import pipeline.
- **Lum.** Определяет package, manifest, spine и Markdown chapters сокращенного
  материала.
- **Search/KB.** Индексирует активные сохраненные artifacts согласно общей
  policy; производный material индексируется как обычный material с provenance.
- **MCP.** Внешний агент может выполнять те же queued summary/abridgement tasks.

## Альтернативы

- `accepted`: быстрое selection summary остается в глобальном чате.
- `accepted`: chapter summary запускается кнопками reader в конце главы.
- `accepted`: один активный summary на source scope без именованных вариантов.
- `accepted`: сокращенная версия входит в scope и создается отдельным
  `.lum`-материалом.
- `rejected`: сохранять каждое краткое chat summary как artifact
  автоматически.
- `rejected`: считать сокращенную версию новой revision оригинального
  материала.

## Принятый профиль `0.2.0`

- Default form: `brief` для главы и `outline` для материала. Пользователь может
  явно выбрать вторую поддерживаемую form до создания task.
- Иерархический progress показывает стабильные стадии:
  `context`, `section_summaries`, `synthesis`, `coverage_check`,
  `artifact_validation`; abridgement дополнительно показывает
  `package_assembly`, `package_validation`, `import`, `publication`.
- Если active summary содержит ручную правку, regeneration всегда создает
  candidate. UI показывает candidate рядом с active и дает принять/отклонить;
  автоматический text merge и silent overwrite не выполняются.
- Material-level action доступно из details/card material и в конце чтения.
  Chapter action остается в конце главы.
- Authoritative derived relationship хранится в Lumi schema, а portable
  provenance generated package — в
  `META-INF/lumi/provenance.json` версии
  `lumi.generated-provenance.v1`. `lum.toml` profile `0.1` не расширяется.

Schema и provenance закреплены в [ADR 0019](../adr/0019-ai-task-run-artifact-schema.md)
и [ADR 0024](../adr/0024-derived-material-provenance.md).
