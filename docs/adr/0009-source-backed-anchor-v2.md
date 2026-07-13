# ADR 0009: Multi-block source-backed anchors и conflict-safe annotations

Status: accepted

## Контекст

Этап 5 S1 переводит annotations из fixture/in-memory path в durable web path.
Старый `Anchor` полностью описывал диапазон внутри одного normalized block, но
browser `Selection` может пересекать несколько соседних блоков. Кроме того,
retry web-команд не должен дублировать annotation после потерянного HTTP-ответа,
а stale edit/delete не должен молча перезаписывать новую версию.

## Решение

- Начало anchor по-прежнему задаётся `node_path` и `text_range.start`.
- Добавляются `end_node_path` и `end_source_locator`; `text_range.end` означает
  Unicode scalar offset внутри конечного блока.
- Для совместимости отсутствие новых полей означает same-block range:
  `end_node_path = node_path`, `end_source_locator = source_locator`.
- Quote много-блочного диапазона собирается из normalized text блоков через
  `\n`; prefix/suffix ограничены контекстом начального/конечного блока.
- Composite `content_hash` много-блочного диапазона строится из ordered block
  hashes. DOM path и текущая page geometry не являются primary selector.
- Browser adapter создаёт anchor только из source-text spans со stable node id
  и scalar fragment offset; synthetic markers и link controls не участвуют.
- Resolver использует conservative ladder: exact path/range/hash, unique quote
  в block, unique quote+context, non-empty source locator+checksum, bounded
  unique fuzzy local match, иначе `Unresolved`.
- Create/update/delete принимают `Idempotency-Key`. PostgreSQL transaction
  сохраняет mutation/tombstone, `sync_changes` и исходный response. Повтор того
  же key+command возвращает исходный response; другой command получает `409`.
- Update/delete проверяют `expected_revision` в SQL predicate. Delete создаёт
  tombstone; stale command не меняет запись.

## Последствия

- Anchor не зависит от темы, размера текста, viewport или derived `PageMap`.
- Multi-block selection переносим между web/WebView/native adapters при той же
  normalized revision.
- Ambiguous recovery остаётся unresolved вместо опасной автоматической
  перепривязки.
- Поля anchor additive для JSON, но domain/sync marker повышен до
  `s1.2026-07-13.anchors-v2`; старые same-block payloads покрыты compatibility
  test.
- PostgreSQL migration добавляет constraints и active indexes, не переписывая
  существующие JSON anchors.

## Отклонённые альтернативы

- DOM Range/path как persisted anchor: ломается при pagination и настройках.
- Один global character offset: теряет normalized/source provenance.
- LWW для notes: может незаметно потерять пользовательский текст.
- Автоматически выбирать первый quote match: неоднозначная цитата может
  привязаться к неверному месту.

## Required fixtures и проверки

- legacy same-block Anchor JSON без end fields;
- Unicode scalar offsets, включая Cyrillic и surrogate-pair emoji;
- exact multi-block selection и ambiguous quote → `Unresolved`;
- replay одного idempotency key, stale update/delete, tombstone/export;
- browser reload, repagination, notes navigation и portable JSON export.
