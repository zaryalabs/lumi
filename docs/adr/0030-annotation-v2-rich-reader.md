# ADR 0030: Annotation v2 и rich overlays Reader

Status: accepted

## Контекст

Baseline S1 хранит source-backed `Annotation` с двумя payload-вариантами:
цветным highlight и Markdown note. Контракт уже гарантирует stable id,
revision-bound anchor, идемпотентные команды, optimistic revision и tombstone,
но не выражает самостоятельные записи на полях, status/tags/title, связь note
с highlight и будущий voice payload. Поле `kind` хранится только как JSONB,
поэтому тип записи и гранулярность цели нельзя безопасно фильтровать без
декодирования payload.

Rich Reader должен добавить жёлтое и жирное смысловое выделение, не нарушив
постраничность. Обычный `font-weight: bold` меняет метрики текста и сделал бы
существующий `PageMap` неверным после создания или редактирования highlight.

## Решение

### Domain и совместимость

- `Annotation` остаётся одной aggregate с прежними `id`, `material_id`,
  `revision_id`, `anchor`, `kind`, `revision` и timestamps.
- V2 добавляет явные `annotation_type`, `target`, `status`, `title`, `tags` и
  `related_annotation_id`.
- `AnnotationTarget` описывает гранулярность устойчивой source-backed цели:
  `text_range`, `block`, `section`, `document` или `page_area`. Canonical
  selector по-прежнему хранится в `anchor`; target не дублирует DOM path.
- Payload `kind` остаётся tagged JSON для совместимости. Он поддерживает
  `highlight`, Markdown `note` и future-compatible `voice_note`. Margin note —
  note с `annotation_type = margin_note` и не-range target.
- Декодер `Annotation` принимает старый JSON без v2-полей, выводит тип из
  старого `kind`, цель из anchor и задаёт `status = active`.
- Старые create/update payload без metadata остаются text-range
  highlight/note. Новые клиенты всегда отправляют v2 metadata.
- Portable export повышается до `lumi.annotations.v2`, содержит полную v2
  запись и сохраняет возможность декодировать `lumi.annotations.v1`.

### Persistence и changes

- Таблица `annotations` расширяется additive columns `annotation_type`,
  `target_kind`, `status`, `title`, `related_annotation_id`,
  `audio_attachment_id` и `payload_schema`.
- Deterministic backfill выводит тип из `kind.type`, а target из page geometry
  и text range существующего anchor. IDs, anchors, object revisions и
  timestamps не изменяются.
- Tags хранятся в отдельной `annotation_tags` с нормализованным ключом и
  стабильным ordinal. `kind` остаётся JSONB variant payload.
- `related_annotation_id` должен указывать на активную запись того же owner и
  material. Voice payload может ссылаться только на активный
  `AudioAttachment` того же owner; bytes в annotation не сохраняются.
- Create/update/delete по-прежнему атомарно пишут primary row и один
  `sync_changes` event. Change payload содержит публичный Annotation v2 DTO и
  не раскрывает note/audio содержимое в operational logs.
- Domain/sync marker повышается до `s1.2026-07-26.records-v2`.

### Bold overlay

- Reflowable Reader использует paint-only bold decoration на уже измеренном
  source span: небольшую обводку glyph (`text-shadow`/text stroke), а не
  `font-weight`.
- Decoration не меняет inline metrics, поэтому `PageMap` cache key и границы
  страниц остаются прежними. Создание, смена yellow/bold и удаление highlight
  не запускают repagination.
- PDF Reader использует отдельный overlay class над сохранёнными page rects;
  PDF text layer и canvas не меняются.
- Если будущая платформа не может дать качественный paint-only результат, она
  должна выбрать controlled repagination и включить revision декораций в cache
  key. Независимый `font-weight` поверх старого `PageMap` запрещён.

## Последствия

- Reader и будущий Desk/search читают один queryable record contract.
- V1 rows и JSON payloads продолжают читаться, но новые export/change payloads
  имеют v2 marker.
- Margin notes получают тот же anchor recovery и conflict-safe lifecycle, что
  заметки к выделению.
- Paint-only bold сохраняет навигацию и source offsets, но визуально может
  немного отличаться от настоящего bold face конкретного шрифта.
- Voice lifecycle, wikilinks/backlinks и Desk projection остаются следующими
  эпиками; schema заранее имеет безопасные seams, но capabilities публикуются
  только после готового пользовательского пути.

## Отклонённые альтернативы

- Новые таблицы для highlights, notes и margin notes: создают несколько
  lifecycle/change contracts и усложняют единый Desk/search.
- Хранить target как DOM path или вычисленную страницу: ломается от настроек,
  viewport и repagination.
- Хранить tags только внутри JSON payload: затрудняет permission-aware
  фильтрацию и индексирование.
- Использовать обычный `font-weight: bold` без invalidation `PageMap`: границы
  страниц становятся недостоверными.
- Молча выбирать foreign/tombstoned related annotation: нарушает owner scope и
  может вести к неверному source.

## Compatibility impact и required fixtures

- legacy v1 highlight/note JSON без v2 metadata;
- PostgreSQL migration/backfill с неизменными ids/anchors/revisions;
- Cyrillic/English title, Markdown body и normalized duplicate tags;
- text-range, block, section, document и PDF page-area targets;
- yellow ↔ bold update с expected revision и retry одного idempotency key;
- foreign/tombstoned relation и audio attachment rejection;
- v2 export round-trip и v1 decoder;
- browser reload, theme/viewport repagination, keyboard-only margin note и PDF
  page-area note.
