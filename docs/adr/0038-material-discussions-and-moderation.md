# ADR 0038: Material discussions и moderation tombstones

Status: accepted

## Контекст

`0.5.0/E3` выполняется параллельно с `0.4.0`. Shared anchors, публикация
private highlight и Reader overlay требуют ещё не готовых Annotation v2,
`LinkTarget` и provenance contracts. При этом обсуждение material identity
целиком не зависит от private record: по принятой social visibility policy оно
доступно active member даже без matched claim.

Нужен самостоятельный срез, который не вводит временный anchor/record contract:

- material-level threads и одноуровневые replies;
- author edit/delete с optimistic concurrency;
- owner/admin hide/restore/delete;
- tombstones и append-only moderation audit;
- cursor delivery без раскрытия source, quote или private annotation.

## Решение

Вводится контракт `material-discussion.v1` и capability
`material-discussions`. Он содержит только `scope = material`.
`shared_comment_threads` не имеет nullable anchor/target placeholder:
anchor-bearing scopes будут добавлены expand migration после принятия общего
Records v2 contract.

Создание thread атомарно создаёт первый comment. Reply может ссылаться только
на видимый top-level comment; глубина ограничена одним уровнем. Thread получает
новую revision и `updated_at` при любом изменении comments, поэтому выдача
может использовать стабильный cursor `(updated_at, thread_id)`. Одна страница
ограничена 100 threads, один thread — 500 неудалёнными comments, body —
16 KiB UTF-8.

Все mutations требуют session, CSRF и `Idempotency-Key`. Edit/delete автора и
moderation используют `expected_revision`. Delete очищает body и оставляет
tombstone. Hide сохраняет body в authoritative table, но обычному member API
возвращает `body_markdown = null`; owner/admin может видеть скрытый body для
moderирования. Moderation action хранится append-only с target/action и
bounded reason.

Material-level read/create требует active membership, но не matched claim:
ответ не содержит текста материала. Anchored bodies в этом контракте
отсутствуют. Cross-Space object lookup маскируется как not found или conflict
по той же boundary, что остальные Community operations.

Успешная mutation записывает `sync_changes` с
`schema_version = material-discussion.v1`. Activity содержит только
payload-free `discussion_started` или `content_moderated`; body и reason не
попадают в application logs или activity payload.

## Последствия

- Два участника могут обсуждать metadata identity до появления shared anchors,
  не получая доступ к чужой копии.
- Private notes/highlights невозможно случайно опубликовать через этот API:
  команды принимают только явный body пользователя.
- Capability `shared-reading` остаётся выключенной. `material-discussions` не
  означает поддержку Reader overlay, quote mapping или shared highlights.
- После готовности `0.4.0` schema расширяется общим target/provenance, а не
  заменяется вторым social-only anchor.

## Compatibility

Forward migration `20260726320000_material_discussions.sql` добавляет новые
таблицы без изменения существующих E1–E2 rows. Rollback выключает capability,
но сохраняет forward schema и tombstones.
