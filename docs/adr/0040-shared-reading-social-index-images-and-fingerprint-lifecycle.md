# ADR 0040: shared reading, social index, Community images и fingerprint lifecycle

Status: accepted

Date: 2026-07-27

## Контекст

После завершения `0.4.0` стали стабильны Annotation v2, общий search runtime,
generic blob lifecycle, durable Job runtime и open-target contracts. Поэтому
отложенные части `0.5.0` больше не должны поддерживать параллельные временные
модели.

Основной риск — случайно перенести в Community личный record: body заметки,
title, tags, private material/revision identity или исходный файл. Второй риск —
вернуть social search result после отзыва membership/claim, пока index event ещё
стоит в очереди.

## Решение

### Shared anchor и Reader layer

- `SharedAnchorDraft` создаётся из Annotation v2 или live selection и поступает
  только в командную границу.
- Сервер проверяет private material, active revision, matched claim и optional
  provenance Annotation. В публичную сущность попадают только target, bounded
  quote/context, heading path и page label.
- `SharedAnchor` не содержит `material_id`, `revision_id`,
  `annotation_id`, title, tags или body личной заметки.
- Threads поддерживают material/section/anchor/page scopes.
- Shared highlight — отдельная сущность. Publish/unpublish не изменяют и не
  удаляют private Annotation.
- При чтении сервер сопоставляет anchor с текущей active revision пользователя.
  Результат всегда явный: `resolved` со стратегией/confidence либо `unresolved`
  с безопасным публичным контекстом.
- Reflowable и PDF Reader рисуют social overlays отдельно от личных annotations.

Capability: `shared-reading`.

### Permission-aware social search

- Shared comments, Space chat и published highlights используют общий
  `SearchChunk`, Tantivy и Job runtime.
- Chunk несёт `community_space_id` и optional `shared_material_id`; Community
  scope фильтруется внутри Tantivy до формирования результата.
- Mutation, membership, claim и moderation transitions создают durable
  permission-bearing index events.
- Перед выдачей результата application service повторно проверяет active
  membership, matched claim для anchor-bearing объектов и moderation state.
  Это закрывает окно между отзывом доступа и обработкой очереди.
- MCP `search_shared_comments` и `search_space_chat` используют тот же service,
  scope и повторную permission-проверку.

Capability: `social-search-index`.

### Avatar и cover

- Community images используют generic content-addressed `BlobStore`, но имеют
  собственные refs и optimistic revision aggregate Space.
- Разрешены только валидные PNG/JPEG до 5 МБ и 4096 px; Content-Type обязан
  совпадать с сигнатурой, а geometry проверяется по slot.
- Download разрешён только active member. Replacement атомарно переключает ref,
  уменьшает старый refcount и оставляет unreferenced blob на 24 часа перед
  cleanup.

Capability: `community-images`.

### Fingerprint lifecycle

- Successful revision activation ставит общий durable
  `Job(kind = material_fingerprint)`.
- Миграция добавляет backfill для всех ready active revisions.
- Worker вычисляет fingerprint текущего normalized package и в той же
  lifecycle-проекции автоматически перепроверяет существующие claims.
- `material-fingerprint.v2` добавляет protected HMAC evidence для ISBN, DOI и
  canonical URL. Сырые identifiers не выходят в shared/API contracts.
- Claim transitions создают search invalidation/rebuild events, поэтому
  open-target и permission lifecycle остаются согласованными.

## Последствия

- Private records остаются owner-scoped; Community получает отдельную
  provenance-запись и отдельные shared entities.
- Index является производной проекцией, а не источником авторизации.
- Unresolved anchor — штатное состояние и не заменяется приблизительным
  переходом без достаточной уверенности.
- Старые fingerprints пересчитываются durable backfill, а не внутри startup
  request.
- Операторы должны сохранять blob root и Job/search tables в backup и
  наблюдать backlog `material_fingerprint` и social permission events.

## Evidence

- migration:
  `20260726340000_deferred_social_reading.sql`;
- pure privacy и cross-Space isolation tests в `lumi-core`/`lumi-server`;
- image signature/geometry tests;
- `make c` и `make web-e2e`;
- актуальный dependency record:
  [`0.5.0-deferred-until-0.4.0.md`](../tmp-plans/0.5.0-deferred-until-0.4.0.md).
