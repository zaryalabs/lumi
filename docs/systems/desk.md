# Desk

Status: accepted

## Контекст

Desk — отдельная верхнеуровневая поверхность User Space для возвращения к
результатам чтения и обучения. Она собирает в одном месте записи, хайлайты,
голосовые заметки, сохраненные производные артефакты, задания и результаты
обучения, группируя их прежде всего вокруг материалов.

Desk не является расширенной боковой панелью reader. Контекстная панель
reader показывает данные текущего материала и помогает не прерывать чтение.
Desk работает между материалами: позволяет обозревать накопленное,
редактировать записи, видеть состояние обучения и возвращаться к точному месту
в источнике.

Desk также не заменяет:

- единый поиск, который начинается с пользовательского запроса;
- базу знаний, организованную вокруг идей и связей между источниками;
- глобальный ИИ-чат, который является процессом диалога;
- библиотеку, которая управляет самими материалами и импортом.

Desk не является Space. User Space обозначает персональную страницу и опыт Lumi
целиком, а Desk — одну рабочую поверхность этого опыта. Community Space
использует собственную collaborative surface, название которой пока не
фиксируется.

Web IA называет общий record-раздел `Заметки и выделения`, сохраняет sort,
status и tag filters при list/detail navigation и показывает доступные теги из
текущей projection page. Технические generation/version значения доступны в
диагностике, но не конкурируют с пользовательскими счётчиками. Ошибка загрузки
предлагает явный retry.

Базовый цикл:

```text
Материал
  -> чтение и обучение
  -> записи / хайлайты / попытки / сохраненные артефакты
  -> Desk по материалу
  -> возврат к anchor, редактирование, повторение или перенос в базу знаний
```

## Принципы

- **Material-centered navigation.** Материал является основной верхней
  группировкой и source context для Desk.
- **Grouping is not ownership.** Desk является проекцией над доменными
  объектами. Не каждая показанная сущность обязана физически принадлежать
  одному материалу: KB note или accepted artifact может ссылаться на несколько
  источников.
- **Source-backed items.** Записи и результаты сохраняют provenance и, где
  применимо, точный `Anchor`.
- **Stable links, readable paths.** Пользователь видит таксономический путь, но
  ссылка разрешается в стабильные ids, а не зависит от изменяемых названий.
- **Saved state, not conversation log.** Raw AI chat messages не входят в
  Desk. Явно сохраненные результаты чата могут стать note, explanation,
  summary, cards или другим типизированным артефактом и после этого появиться в
  Desk.
- **Meaningful state, not telemetry.** Сырые `ReadingEvent` не показываются как
  пользовательские записи. Desk использует агрегаты: прогресс, последнее
  чтение, завершенные разделы и learning state.

## Пользовательские сценарии

- Пользователь открывает Desk и видит материалы, по которым у него есть
  записи, результаты обучения или сохраненные артефакты.
- Пользователь открывает Desk материала и получает обзор прогресса,
  последних записей, незавершенных заданий и состояния повторения.
- Пользователь просматривает все хайлайты, текстовые и голосовые заметки по
  материалу, сгруппированные по главам, разделам или страницам.
- Пользователь редактирует заметку непосредственно в Desk, не открывая
  reader для простого изменения текста, тегов или статуса.
- Пользователь открывает запись в reader точно на исходном anchor и может
  переходить к соседним записям того же материала.
- Пользователь просматривает созданные тесты, прошлые попытки, результаты,
  пропущенные задания, карточки и explain-back/reflection artifacts.
- Пользователь переключает ось навигации: сначала материал, затем тип; либо
  сначала тип или состояние, затем материал.
- Пользователь фильтрует все записи по типу, тегу, статусу, времени, материалу
  и наличию собственного комментария.
- Пользователь связывает запись с материалом, anchor, другой записью,
  learning item, summary или KB note через wikilink-like picker.
- Пользователь превращает reader note или highlight в KB note, не теряя source
  refs и обратную ссылку.
- Пользователь сохраняет полезный ответ ИИ как типизированный артефакт; только
  после этого он появляется в Desk.

## Функциональные требования

### Верхнеуровневые поверхности

Desk должен поддерживать два взаимодополняющих входа:

1. **Все материалы.** Список или дерево материалов с количеством записей,
   состоянием обучения, последней активностью и доступными фильтрами.
2. **Сквозные представления.** Все записи, голосовые заметки, нужно повторить,
   незавершенные задания, сохраненные summaries и другие типовые срезы,
   сгруппированные по материалам.

Открытие материала дает Desk материала:

```text
Материал
  -> Обзор
  -> Записи
  -> Обучение
  -> Сохраненные артефакты
```

Точные названия разделов являются UI-copy и могут уточняться. Контракт данных
не должен зависеть от конкретной desktop/sidebar или mobile/tab компоновки.

### Обзор материала

Обзор показывает компактное, полезное для возврата состояние:

- reading progress и последнюю осмысленную позицию;
- дату последнего чтения;
- количество записей по типам;
- последние измененные записи;
- активные или просроченные learning items;
- последние попытки и агрегированное состояние mastery;
- доступные summaries и другие сохраненные artifacts;
- unresolved anchors, conflicts или missing audio/transcript states, если они
  требуют внимания пользователя.

Сырые timeline events не выводятся как самостоятельная лента по умолчанию.

### Записи

В раздел записей входят:

- highlights, включая highlights без пользовательского комментария;
- notes к выделенному фрагменту;
- margin notes;
- voice notes и доступные transcripts;
- bookmarks, если пользователь включил их в фильтр;
- reader-derived KB notes как связанные объекты, а не как дубликаты.

Записи группируются:

- по материалу;
- внутри материала по `ContentUnit`, heading path, главе или PDF page;
- внутри структурной группы по anchor order;
- для записей без точного anchor — по material-level group.

Desk должен уметь отличать:

- highlight без комментария;
- highlight с note;
- самостоятельную margin note;
- voice note без транскрипта;
- voice note с черновым или отредактированным транскриптом;
- unresolved annotation, которую нельзя надежно показать на текущей revision.

### Обучение

В Desk материала входят устойчивые learning objects и результаты:

- quizzes, open questions, flashcards, cloze и hinted questions;
- chapter/material tests;
- attempts и результаты;
- skipped, missed, due и completed states;
- explain-back и reflection artifacts;
- mastery и scheduling summary;
- вручную созданные и принятые generated learning items.

Desk не заменяет специализированный challenge/review flow. Он показывает
историю и состояние обучения вокруг материала, а активную сессию запускает в
learning surface.

### Сохраненные артефакты

Раздел может показывать:

- material/chapter summaries;
- accepted explanations;
- accepted question/card sets;
- accepted entity/concept/link artifacts;
- note drafts, если они требуют пользовательского решения;
- transcripts, если они представлены отдельным artifact.

Rejected drafts и промежуточные результаты задач не показываются по умолчанию.
Queued/running/failed task state относится к task queue, кроме случаев, когда
пользователю нужно исправить конкретный Desk item.

### Редактирование и действия

Desk поддерживает:

- редактирование note body, title, tags, status и manual classification;
- изменение highlight style/category;
- воспроизведение voice note и редактирование готового transcript;
- удаление и восстановление там, где retention policy это допускает;
- переход к source anchor;
- переход к связанному объекту;
- создание или удаление links;
- преобразование записи в KB note или вставку в существующую KB note;
- запуск повторения или новой learning attempt;
- экспорт выбранной записи, группы или Desk материала.

Изменения используют те же domain commands, revisions, idempotency и conflict
handling, что reader, KB и learning surfaces. Desk не создает отдельные
копии объектов для редактирования.

### Маршрутизация

Клиентская маршрутизация должна поддерживать прямые ссылки как минимум на:

```text
/desk
/desk/records
/desk/learning
/desk/material/:material_id
/desk/material/:material_id/records
/desk/material/:material_id/learning
/desk/item/:object_type/:object_id
```

Exact URL shape является adapter detail, но route state должен быть
восстанавливаемым после reload и пригодным для browser history.

Переход к source использует `material_id + anchor` и открывает reader на
актуальной `DocumentRevision`. Если anchor unresolved, Desk сохраняет
запись доступной и показывает recovery state вместо потери объекта.

### Таксономия и внутренние ссылки

Пользовательский путь строится из доступного контекста:

```text
Название материала / Записи / Название записи
Название материала / Обучение / Тест по главе
Название материала / Глава / Фрагмент
```

Он может отображаться и вводиться как wikilink:

```text
[[Название материала/Записи/Репликация]]
[[Название материала/Обучение/Тест по главе 5]]
[[Название материала#Глава 5]]
```

Путь является display/lookup representation, а не primary key. После
разрешения Lumi хранит стабильную цель:

```text
LinkTarget {
  object_type
  object_id
  material_id?
  anchor?
  display_path
}
```

Базовый record-link contract поддерживает material, anchor и annotation.
Learning item, attempt, saved artifact и KB note добавляются как новые target
types соответствующими подсистемами без изменения существующих ids.
Переименование материала, записи или раздела обновляет отображаемый путь, но
не ломает разрешенную ссылку.

Если цель пока не найдена или неоднозначна, ссылка хранится как unresolved
reference с исходным текстом. Autocomplete и picker должны предпочитать
стабильные существующие объекты.

Этот внутренний link model не зависит от интеграции с Obsidian. Будущий
Markdown/Obsidian export проецирует стабильные Lumi links в читаемые wikilinks,
front matter и optional deep links.

### Фильтры и сортировка

Минимальные фильтры:

- material;
- object type;
- structural group/chapter/page;
- tags;
- status;
- created/updated range;
- has user note;
- has transcript;
- unresolved/conflicted;
- learning state: due, missed, skipped, completed.

Сортировки:

- source order;
- last updated;
- created time;
- material title;
- learning due time;
- relevance when Desk is opened from search.

Выбранные фильтры должны быть отражены в route/query state, чтобы view можно
было восстановить или передать между клиентскими поверхностями.

### Граница с ИИ-чатом

Raw `AiChat`, `AiChatMessage` и незакрепленный selection context не входят в
Desk.

Явное действие пользователя может преобразовать результат чата в:

- `Note`;
- `ExplanationArtifact`;
- `SummaryArtifact`;
- `QuestionSetArtifact`;
- `FlashcardSetArtifact`;
- `KbNoteDraft`;
- другой принятый типизированный artifact.

После сохранения объект получает provenance, source refs и появляется в
соответствующем разделе Desk. Ссылка на исходный chat может сохраняться
как provenance, но Desk не становится вторым интерфейсом истории чата.

### Граница с базой знаний

Desk организован вокруг источников и истории работы с ними. База знаний
организована вокруг идей и связей между несколькими источниками.

Один объект может быть видим в обеих поверхностях через projection:

- reader note видна в представлении соответствующего материала в Desk;
- после преобразования в KB note она также видна в KB;
- KB note с несколькими `KbSourceRef` может показываться как связанный объект
  в представлениях нескольких материалов в Desk;
- физическое дублирование текста для этого не требуется.

## Нефункциональные требования

- **Навигационная устойчивость.** Direct routes восстанавливаются после reload,
  а stable target links переживают переименование display paths.
- **Производительность.** Списки должны использовать pagination/cursors,
  агрегированные counters и виртуализацию; открытие Desk не должно
  загружать полный текст всех материалов.
- **Консистентность.** Reader, Desk, KB и learning surfaces редактируют
  одни и те же domain objects и revision history.
- **Source fidelity.** Любой source-backed item сохраняет provenance и anchor,
  даже если anchor временно unresolved.
- **Доступность.** Группы, фильтры, счетчики и действия доступны через
  семантические headings, labels, lists и keyboard navigation.
- **Privacy.** Desk работает с personal scope. Private notes не появляются в
  Community Space или shared material views без явной публикации.
- **Rebuildability.** Desk counters, groups и navigation projections
  являются derived data и перестраиваются из primary domain state.
- **Cross-platform.** Web является первым target. Desktop/mobile позже
  используют тот же query/link contract поверх full-copy replicas.

## Модель данных

Desk по умолчанию не вводит новый aggregate root. Он строит read models
над существующими объектами:

```text
Material
  <- Annotation / Highlight / Note / VoiceNote / Bookmark
  <- LearningItem / LearningAttempt / MasteryState
  <- AcceptedArtifact / Summary / Transcript
  <- KbSourceRef / linked KbNote
  <- ReadingProgress aggregates
  -> DeskMaterialProjection
  -> DeskItemProjection[]
```

Предварительные projection contracts:

```text
DeskMaterialProjection {
  material_id
  title
  source_type
  cover_ref?
  reading_progress
  last_read_at?
  last_activity_at?
  record_counts
  learning_counts
  artifact_counts
  attention_states
}

DeskItemProjection {
  object_type
  object_id
  material_refs[]
  primary_material_id?
  source_anchor?
  structural_path?
  display_title
  preview
  tags
  status
  created_at
  updated_at
  sort_key?
  capabilities
}
```

`material_refs[]` позволяет показывать один knowledge/artifact object вокруг
нескольких источников без копирования primary object.

## Реализация

Web-срез `0.4.0/E4` реализован по
[`ADR 0034`](../adr/0034-desk-projection-and-search-surfaces.md):

- `desk.contract.v1` находится в `lumi-core`;
- migration `20260726290000_desk_projection.sql` создаёт versioned
  owner-scoped material/item projection и deterministic rebuild;
- `/api/v1/desk/materials`, `/desk/items` и detail routes возвращают bounded
  cursor pages, exact open targets и читают отображаемый payload из primary
  objects;
- Web публикует top-level Desk, material/records/learning/artifact/item
  direct routes, filters/sorts, inline Annotation edit и переходы в Reader/
  Challenges;
- MCP registry `mcp-tools.v3` использует тот же `DeskRuntime`, account scope,
  cursors и limits.

Эксплуатация и rebuild описаны в
[`Desk runbook`](../runbooks/desk-projection.md).

### Query boundary

Backend/application layer должен предоставлять material-centered read queries,
не связывая их с конкретной UI-компоновкой:

```text
list_desk_materials(filters, sort, cursor)
get_material_desk(material_id)
list_desk_items(scope, filters, sort, cursor)
get_desk_item(object_type, object_id)
resolve_desk_link(text, context)
```

Web использует server-side projections над account state. Будущие native
clients строят совместимую projection над локальной full-copy replica.

### Projection updates

Projection/counters обновляются после изменений:

- annotations и anchors;
- reading progress;
- learning items, attempts, mastery и scheduling;
- artifact acceptance/status;
- KB source refs;
- material metadata или document revision;
- deletion/tombstone/conflict state.

Projection должна допускать полный rebuild. Primary notes, attempts и artifacts
не хранятся только внутри Desk index.

### Link resolution

Link resolver:

1. Разбирает пользовательский путь и optional type qualifier.
2. Ищет цели в допустимом account/scope.
3. Использует текущий material context как ranking boost.
4. При однозначном выборе сохраняет stable `LinkTarget`.
5. При неоднозначности предлагает picker.
6. При отсутствии цели сохраняет unresolved reference.
7. При переименовании пересчитывает display path без изменения target id.

## Интеграции и зависимости

- **Reader.** Контекстная панель остается material-local; Desk дает
  межматериальный обзор и открывает reader на source anchor.
- **Библиотека.** `Material` metadata и lifecycle задают верхнюю группировку,
  но наличие материала в библиотеке не означает наличие Desk activity.
- **Обучение.** Desk показывает persistent learning state и историю;
  challenge/review surface проводит активную сессию.
- **База знаний.** KB notes связываются через source refs и могут отображаться
  в представлениях нескольких материалов в Desk без дублирования.
- **Поиск.** Search может открыть Desk с query/filter state или сразу
  перейти к Desk item/source anchor.
- **ИИ.** Только сохраненные typed artifacts входят в Desk; raw chat и
  промежуточный dialogue state исключены.
- **Синхронизация.** Primary objects синхронизируются; Desk projection
  перестраивается локально или на сервере.
- **Obsidian.** Не является зависимостью Desk. Поздняя Desktop-интеграция
  экспортирует внутренние links и source refs в Markdown projection.
- **MCP.** Внешние агенты должны уметь перечислять Desk items по account,
  material и type scopes, не ограничиваясь одним `material_id`.

## Альтернативы

- `rejected`: расширить reader side panel до глобальной панели. Reader panel
  должна сохранять текущий контекст и не превращаться в отдельное приложение
  внутри reader.
- `rejected`: использовать единый поиск как единственный способ возвращения к
  записям. Search требует запроса и не дает material-centered overview,
  editing и learning state.
- `rejected`: объединить Desk и базу знаний. У них разные primary axes:
  источник и история работы против идей и cross-source связей.
- `rejected`: включить raw AI chat history. Это смешивает процесс диалога с
  устойчивыми пользовательскими результатами и создает шум.
- `rejected`: использовать display taxonomy path как primary id. Rename и
  reclassification ломали бы ссылки.
- `accepted`: Desk как rebuildable material-centered projection над
  annotations, learning state, accepted artifacts и source-linked KB objects.

## Открытые вопросы

- Нужны ли bookmarks в разделе «Записи» по умолчанию или только через фильтр?
- Какой default view лучше для `/desk`: материалы, последние записи или
  состояния, требующие внимания?
- Какие learning aggregates достаточно полезны для material overview без
  превращения Desk в аналитический dashboard?
- Нужна ли отдельная user-managed taxonomy сверх material/type/tags?
- Какие типы записей допускают inline editing на mobile, а какие должны
  открываться на отдельном экране?
