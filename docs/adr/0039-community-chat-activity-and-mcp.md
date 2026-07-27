# ADR 0039: Community chat, activity и MCP parity

Status: accepted

Дата: 2026-07-26

## Контекст

Community Space уже имеет authoritative membership, material identity и
material-level discussions. Для выпуска `0.5.0/E4` нужны общий chat,
системная activity, bounded Web delivery и account-scoped MCP adapters.
Realtime transport и общий permission-aware search runtime ещё не готовы.

Chat нельзя смешивать с material discussion: у них разные контекст, lifecycle
и пользовательское ожидание. Activity является производной системной лентой и
не может становиться источником authorization или material state.

## Решение

1. `shared_chat_messages` хранит member-only сообщения отдельно от comments.
   Active member может создать сообщение и изменить/удалить только своё.
   Owner/admin применяет общий hide/restore/delete moderation contract.
2. Delete очищает body и оставляет tombstone. Hide маскирует body для обычного
   участника; moderator получает body для разбора и восстановления.
3. Все chat mutations атомарно записывают materialized row, `sync_changes` и
   idempotent response. Создание дополнительно добавляет allowlisted activity
   event без body.
4. `shared_activity_events` остаётся append-only projection. Public DTO
   содержит только allowlisted `kind`, безопасный subject id/type, actor и
   timestamp; payload из БД наружу не выдаётся.
5. Chat и activity читаются cursor pages не более 100 объектов с индексом
   `(community_space_id, created_at, id)`. Cursor не является разрешением:
   active membership проверяется на каждом запросе.
6. Web использует bounded polling: пять секунд при видимой вкладке, пауза в
   hidden tab, увеличенный интервал после ошибки и ручной retry. WebSocket не
   входит в первый release.
7. MCP registry `mcp-tools.v4` добавляет Community list/detail, share,
   material comments и chat tools. Они вызывают те же `SocialRuntime`
   application services, permission checks, idempotency и cursor limits, что
   REST/Web. Forbidden resource маскируется как not found.
8. Capability `community-communications` публикуется только persistent
   server. `social-search-index` остаётся выключенной до общего search runtime
   `0.4.0`.

## Privacy и observability

- Chat/comment bodies, invite tokens, fingerprints и private source metadata
  не попадают в activity, sync envelopes или structured logs.
- Логи mutations содержат только operation, actor id, Space id, object id и
  result.
- Membership removal действует на следующий REST/MCP read; cached cursor не
  продлевает доступ.
- Backup/restore row-count evidence включает community memberships, revoked
  links, shared materials, comments, chat, activity и moderation actions.

## Последствия

- Polling создаёт ограниченную предсказуемую нагрузку и сохраняет простой
  transport contract, но не обещает realtime latency.
- Activity можно перестроить только из части domain commands, поэтому она не
  используется для восстановления authoritative состояния.
- Permission-bearing search events и их invalidation добавляются после
  `0.4.0/E3–E5`, без временного social-only индекса.
