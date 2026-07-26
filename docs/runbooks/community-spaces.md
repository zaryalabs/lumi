# Community Spaces

## Назначение

Runbook описывает локальную проверку `0.5.0/E1–E3 independent`: закрытые Community Spaces,
membership/roles, доступ по отзывной ссылке, публикацию безопасной material
identity, привязку собственной копии и material-level discussions. Shared
anchors/highlights, Social Reader overlay, chat и social search пока не входят
в опубликованные capabilities.

## Capabilities

Persistent и memory server публикуют route groups `spaces`, `shares` и
capabilities `community-spaces`, `community-link-access`. Только persistent
server дополнительно публикует `material-sharing`, потому что matching требует
PostgreSQL projection, normalized packages и versioned feature key. Web
показывает действия публикации только после получения этой capability.
Persistent server также публикует `material-discussions`: material-level
threads не требуют claim и не содержат quote/source body. Capability
`shared-reading` остаётся выключенной.

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

Invitation передаётся как `#join/{token}`. Fragment не отправляется серверу
браузером; Web передаёт token только в preview/join API и очищает route после
успешного вступления.

## Автоматические проверки

```sh
cargo test -p lumi-core social
cargo test -p lumi-server social
npm --prefix tests/e2e run test -- social.spec.ts
make c
make web-e2e
```

PostgreSQL integration tests ожидают чистую мигрированную test database через
`LUMI_TEST_DATABASE_URL`. E2E использует изолированные browser contexts и
проверяет create → preview → join → revoke/remove, а также
share → no-copy → exact match/manual-review.

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
- Discussion DTO не содержит anchor/quote/private record. Hidden body
  маскируется для member; delete очищает body и оставляет tombstone.
- Discussion mutations требуют idempotency и expected revision; moderation
  разрешена только owner/admin и записывается append-only.
- Rate limit возвращает `429`, invalid mutation — `422`.

## Отложенные зависимости

Scope, зависящий от contracts `0.4.0`, зафиксирован в
[`../tmp-plans/0.5.0-deferred-until-0.4.0.md`](../tmp-plans/0.5.0-deferred-until-0.4.0.md).
