# Community Spaces

## Назначение

Runbook описывает локальную проверку полного `0.5.0/E1–E4`: закрытые Community Spaces,
membership/roles, доступ по отзывной ссылке, публикацию безопасной material
identity, привязку собственной копии, material-level discussions, Space chat,
activity, shared anchors/highlights, Social Reader overlay, permission-aware
social search, avatar/cover и MCP parity.

## Capabilities

Persistent и memory server публикуют route groups `spaces`, `shares` и
capabilities `community-spaces`, `community-link-access`. Только persistent
server дополнительно публикует `material-sharing`, потому что matching требует
PostgreSQL projection, normalized packages и versioned feature key. Web
показывает действия публикации только после получения этой capability.
Persistent server также публикует `material-discussions`: material-level
threads не требуют claim и не содержат quote/source body. Capability
`community-communications` включает chat/activity REST и Web polling.
`shared-reading` включает cross-copy Reader layer, а `social-search-index` —
общий search runtime и MCP search tools. `community-images` включает
member-authorized avatar/cover lifecycle.

## Ручной сценарий

1. Запустить стек через `make up`.
2. Аккаунтом A открыть `#/community`, создать Space и создать invite link.
3. Открыть ссылку в отдельном browser context под аккаунтом B.
4. Убедиться, что preview показывает только имя, описание и число участников,
   а membership появляется только после явного `Вступить`.
5. Аккаунтом A сменить роль B, затем удалить B. Следующий detail read B должен
   вернуть not found, а повторный join той же общей ссылкой — forbidden.
6. Создать новую ссылку, проверить rotate и revoke: старый token перестаёт
   открывать preview немедленно.
7. Аккаунтом A импортировать Markdown и через меню карточки опубликовать его в
   Space. Confirmation должен явно перечислять только metadata/fingerprint и
   не обещать передачу файла.
8. Аккаунтом B открыть Space: до привязки видна только metadata shell и
   `Импортируйте свою копию`. Импортировать тот же Markdown, подключить его и
   получить `Есть ваша копия`.
9. Третьим аккаунтом импортировать короткий другой текст с тем же названием:
   после подключения состояние должно быть `Нужно подтвердить`, а не matched.
10. A открыть обсуждение material identity и создать thread. B должен увидеть
    его даже без matched claim, ответить, изменить и удалить собственный
    comment.
11. Owner/admin скрывает reply: B видит tombstone-like placeholder без body.
    Restore возвращает body, delete очищает его необратимо и сохраняет
    moderation audit.
12. B пишет в отдельный Space chat. A видит сообщение, скрывает и
    восстанавливает его; activity отдельно показывает allowlisted событие без
    body.
13. Открыть Space в двух вкладках: polling обновляет chat/activity, прекращает
    запросы в hidden tab, после ошибки использует backoff и ручной retry.
14. Через MCP проверить `list_community_spaces`, comment и chat tools. После
    удаления membership следующий вызов с прежним cursor возвращает not found.
15. Создать личную note и highlight к фрагменту, открыть Social Reader panel,
    начать anchor discussion и явно опубликовать highlight. У другого
    участника с matched copy должны появиться overlay и переход; private body,
    title и tags не должны попасть в DTO или search.
16. Импортировать изменённую копию: уверенное место должно восстановиться на
    active revision, а неоднозначное — отображаться как `unresolved`.
17. Выполнить MCP `search_shared_comments` и `search_space_chat`. После
    membership removal, claim transition и moderation hide результат должен
    исчезнуть даже до обработки накопившегося index backlog.
18. Загрузить PNG/JPEG avatar и cover. Проверить member download, запрет
    постороннему, replacement, delete и optimistic conflict со старой revision.
19. Активировать новую revision и проверить durable
    `material_fingerprint` job, автоматическую re-evaluation claim и protected
    ISBN/DOI/canonical URL evidence без сырых identifiers в API.

Invitation передаётся как `#join/{token}`. Fragment не отправляется серверу
браузером; Web передаёт token только в preview/join API и очищает route после
успешного вступления.

## Автоматические проверки

```sh
cargo test -p lumi-core social
cargo test -p lumi-server social
cargo test -p lumi-server search
npm --prefix tests/e2e run test -- social.spec.ts
make c
make web-e2e
```

PostgreSQL integration tests ожидают чистую мигрированную test database через
`LUMI_TEST_DATABASE_URL`. E2E использует изолированные browser contexts и
проверяет create → preview → join → revoke/remove, а также
share → no-copy → exact match/manual-review.
Вторая часть social E2E проверяет chat/activity, moderation и two-context
delivery. `make performance` создаёт 50 000 chat messages и 100 000 activity
events и проверяет first-page budget 300 мс.

## Security и observability

- API различает `CommunitySpaceId` и `SyncSpaceId`.
- Forbidden cross-Space reads маскируются как not found.
- В БД хранится SHA-256 token hash; replay create/rotate использует
  purpose-bound encrypted `SecretStore`.
- Raw token не попадает в idempotency response storage, sync changes, activity
  и application logs.
- Remove/revoke проверяются на каждом server read; browser cache не является
  authorization source.
- Social API принимает только owner-scoped `material_id`; fingerprint, score и
  status нельзя подложить с клиента.
- Raw/protected signatures не входят в HTTP DTO, sync payload или activity;
  source/package routes сохраняют personal-owner scope.
- Material-level Discussion DTO не содержит private record. Shared anchor DTO
  содержит только bounded public context; private provenance хранится отдельно.
  Hidden body
  маскируется для member; delete очищает body и оставляет tombstone.
- Discussion mutations требуют idempotency и expected revision; moderation
  разрешена только owner/admin и записывается append-only.
- Chat использует отдельные rows/DTO; delete очищает body, hide маскирует его
  для member. Activity не возвращает payload и никогда не является ACL source.
- Structured chat traces содержат только operation, actor/Space/object id и
  result.
- Vendor-neutral alert `lumi-community-communications-failures` отслеживает
  устойчивые `rate_limited`/`unavailable` результаты без content bodies.
- Rate limit возвращает `429`, invalid mutation — `422`.
- Social search повторно проверяет membership/claim/moderation после Tantivy и
  не использует index как ACL source.
- Image download проверяет active membership; Content-Type, signature,
  dimensions и slot geometry проверяются до смены ref.

## Backup/restore

`scripts/backup.sh` и `scripts/restore-drill.sh` сверяют community row counts:
Spaces, memberships, все access links и отдельное число revoked links,
shared materials/comments/chat/activity/moderation. PostgreSQL custom dump
остаётся authoritative snapshot, поэтому chat и activity восстанавливаются
согласованно с membership state.

## Release evidence

Закрытый dependency record:
[`../tmp-plans/0.5.0-deferred-until-0.4.0.md`](../tmp-plans/0.5.0-deferred-until-0.4.0.md).
Durable решение:
[`ADR 0040`](../adr/0040-shared-reading-social-index-images-and-fingerprint-lifecycle.md).
