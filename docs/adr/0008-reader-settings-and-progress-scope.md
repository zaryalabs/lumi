# ADR 0008: Scope reader settings и durable position

Status: accepted

## Контекст

Рабочий web-reader пересчитывает страницы при изменении viewport и типографики.
Нужно определить, какие данные принадлежат аккаунту и синхронизируются, а какие
являются производной локальной раскладкой конкретного browser/device.

## Решение

- `ReaderSettings` для S1 являются account-wide replacement value: тема,
  размер текста, line height и preset ширины строки сохраняются в личном
  `sync_space` и применяются после повторного входа на другом web-device.
- `ReadingProgress` сохраняется для пары `space + material` как source-backed
  `Anchor` конкретной `DocumentRevision` и приблизительная доля прогресса.
- Изменения settings и progress принимают `Idempotency-Key`, увеличивают
  `object_revision` и добавляют `sync_changes` с типами `reader_settings` и
  `reading_progress`.
- `PageMap`, номер страницы, viewport bucket и измеренные rects являются
  device-local derived cache. Они не записываются в PostgreSQL и не попадают в
  sync log.
- После смены settings/viewport и после reload reader находит страницу по
  `node_path + Unicode scalar offset`, а не по сохранённому номеру страницы.
- Web client debounce-ит быстро следующие друг за другом settings/progress
  commands, чтобы поздний ответ старого значения не перезаписал новое.

## Последствия

- Предпочтения чтения единообразны внутри аккаунта, а page count может честно
  отличаться между устройствами.
- Восстановление позиции устойчиво к локальному repagination, но смена active
  revision в будущем потребует общего anchor resolver из последующих этапов.
- Device-specific overrides можно добавить отдельным слоем, не меняя durable
  account-wide default.

## Compatibility

- PostgreSQL constraints и index добавлены миграцией
  `20260713200000_stage4_reader.sql`.
- Shared schema marker: `s1.2026-07-13`.
- `ReadingLink` добавлен в normalized nodes как backward-compatible поле с
  пустым default; importer version повышен до `s1.2`.
- Browser pagination следует ADR 0006 и кешируется только в памяти web adapter.
