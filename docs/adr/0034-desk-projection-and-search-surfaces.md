# ADR 0034: Desk projection и единые поисковые поверхности

Status: accepted

## Контекст

Records, learning state и сохранённые AI-артефакты уже принадлежат своим
доменным агрегатам. Для material-centered Desk нужны быстрые счётчики, срезы и
прямые ссылки, но новая копия primary data привела бы к расхождению Reader,
Desk, Web и MCP. Поисковое ядро `0.4.0/E3` уже задаёт owner-filtered query
contract, поэтому отдельный поиск для каждой Web-поверхности также не нужен.

## Решение

1. `desk.contract.v1` определяет `DeskMaterial`, `DeskItem`, cursor pages,
   object/learning states, сортировки и exact `SearchOpenTarget`.
2. PostgreSQL-таблицы `desk_material_projection`,
   `desk_item_projection` и `desk_projection_state` содержат только
   перестраиваемые идентификаторы, counters, filters, revisions и navigation
   metadata. Текст заметки, learning payload и artifact payload читаются из
   primary таблиц при material/item query.
3. Триггеры обновляют projection после изменений material, Annotation v2,
   links/tags, learning item/schedule/attempt и accepted AI artifact. Функция
   `lumi_rebuild_desk_projection(user_id)` полностью воспроизводит owner-scoped
   derived state.
4. HTTP boundary публикует `/api/v1/desk/materials`,
   `/desk/materials/{id}`, `/desk/items`, `/desk/items/{type}/{id}` и
   `/desk/rebuild`. Все запросы начинают с authenticated account scope и имеют
   bounded cursor pagination.
5. Web использует один типизированный hash router для Desk, Search и Reader
   anchor. Route state фильтров и выбранного item восстанавливается после
   reload и browser back/forward.
6. Global, Library, Reader и Desk search вызывают общий `SearchRuntime`.
   Material scope использует Reader ranking, records scope — Records ranking,
   global scope — Global ranking. UI показывает lifecycle индекса и
   объяснимые score/match metadata.
7. MCP registry `mcp-tools.v3` добавляет Desk и search tools. Они вызывают те
   же `DeskRuntime`/`SearchRuntime`, используют те же cursors, limits,
   capabilities, open targets и account permissions, что HTTP/Web.
8. Inline edit в Desk вызывает обычную optimistic Annotation command. Desk не
   изменяет primary таблицы напрямую.

## Последствия

- Reader и Desk показывают одну revision записи, а projection можно удалить и
  восстановить без потери пользовательских данных.
- Новый material виден в Desk даже с нулевыми counters; learning/artifact
  state появляется без отдельного Desk write API.
- Недоступность fastText/index отключает только query capabilities и честно
  отображается в Search; Desk и primary CRUD продолжают работать.
- Query detail выполняет дополнительное чтение primary payload. Списки
  остаются bounded; при больших объёмах потребуется batch enrichment вместо
  изменения контракта.
- Нативные full-copy клиенты смогут построить совместимую локальную projection
  и сохранить те же DTO/routes/open targets.

## Альтернативы

- `rejected`: хранить отдельные Desk-копии note body, learning и artifact
  payload — появляется второй authoritative контур и сложная conflict policy.
- `rejected`: выполнять counters и все срезы live join-запросами — дорогой
  material overview и нет явного rebuild/version состояния.
- `rejected`: отдельный search implementation для Reader или Desk — разные
  permission/ranking/open-target semantics.
- `rejected`: MCP-specific query logic — ломает Web/MCP parity.

## Совместимость

- Forward-only migration `20260726290000_desk_projection.sql` additive и не
  меняет primary domain payload.
- Domain marker повышен до `s1.2026-07-26.desk-search-v1`, migration catalog —
  до `s1-0024-desk-projection`.
- Frozen MCP fixture находится в
  `tests/fixtures/mcp/contracts/v3/tool-registry.json`.
- Required tests: route round-trip/reload, owner isolation, projection rebuild,
  Web/MCP query parity, search capability lifecycle и Reader/Desk shared edit.
