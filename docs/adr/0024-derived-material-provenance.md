# ADR 0024: provenance производного `.lum`-материала

Status: accepted

## Контекст

Сокращение книги создает отдельный material, но должно сохранять связь с exact
original revision и anchors. Provenance нужен одновременно для owner-scoped
navigation внутри Lumi, portable export и безопасной публикации только после
полной validation. Текущий `.lum` profile `0.1` строго проверяет `lum.toml`,
поэтому добавление неизвестной top-level section сломает совместимость.

## Решение

- Сокращение публикуется как отдельный `Material` со своим immutable
  `DocumentRevision`, progress и annotations. Revision оригинала никогда не
  изменяется этим workflow.
- Authoritative server relation `material_derivations` содержит owner,
  `derived_material_id`, `derived_revision_id`, `source_material_id`,
  `source_revision_id`, `kind = abridgement`, producing task/artifact,
  provenance schema version и timestamps.
- Каждая сокращенная глава/section хранит mapping к одному или нескольким
  `SourceCitation` из exact source context pack. Navigation к оригиналу
  разрешается через общую anchor model и повторную permission check.
- Portable package сохраняет минимальный provenance в зарезервированном
  `META-INF/lumi/provenance.json` с marker
  `lumi.generated-provenance.v1`, source material/revision ids, kind,
  generator/prompt/schema versions и bounded source refs. `lum.toml` версии
  `0.1` не расширяется.
- Старый importer может игнорировать reserved metadata и прочитать книгу как
  обычный `.lum`. Новый importer проверяет JSON schema, checksums и ids, но не
  доверяет portable ids как разрешению на доступ к локальному original.
- Abridgement workflow имеет одну parent `AiTask`; per-chapter units являются
  internal Job data. Package собирается во временный blob, проходит обычный
  constrained ZIP/manifest/Markdown validation и ordinary import service.
- `Material` и relation становятся видимыми одной транзакцией только после
  успешного import/publication. Failed/cancelled workflow удаляет или
  quarantines временные blobs и не создает library entry.
- Повторная генерация может создать новую revision существующего derived
  material только при exact same derivation identity; иначе создается новый
  derived material. Новая source revision помечает прежнюю связь `source_changed`
  и не запускает regeneration автоматически.
- Export включает исходный `.lum` package и portable provenance. Удаление или
  потеря доступа к original не удаляет derived material, но переходы к source
  становятся недоступными.

## Последствия

- Производная книга читается обычным reader и остается переносимой.
- Relational provenance дает надежные owner queries, а reserved metadata
  сохраняет связь за пределами конкретной database.
- Compatibility текущего strict `lum.toml` profile не ломается.
- Внешний MCP agent и internal provider возвращают одинаковый chapter/source
  mapping; сборкой, validation и publication всегда владеет Lumi.

## Альтернативы

- Сделать сокращение новой revision original: отклонено, потому что меняется
  смысл source material и его progress/annotations.
- Хранить provenance только в PostgreSQL: отклонено из-за потери при export.
- Добавить `[generated]` в строгий `lum.toml` 0.1: отклонено как breaking
  format change.
- Доверять готовому `.lum` от агента и сразу публиковать: отклонено; package
  обязан пройти обычный validation/import pipeline.

## Совместимость и проверки

- Golden package проверяет старый import без знания provenance, новый
  provenance validator, checksum tampering и original anchor navigation.
- Transaction tests проверяют отсутствие partial material, retry/idempotency,
  source_changed и неизменность original revision.
- Исполняемый assembly/re-import probe находится в
  `spikes/stage0/src/generated_lum.rs`.
