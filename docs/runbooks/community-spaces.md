# Community Spaces

## Назначение

Runbook описывает локальную проверку `0.5.0/E1`: закрытые Community Spaces,
membership/roles и доступ по отзывной ссылке. Материалы, social Reader,
сообщения и social search пока не входят в опубликованные capabilities.

## Capabilities

Persistent и memory server публикуют route groups `spaces`, `shares` и
capabilities `community-spaces`, `community-link-access`. Web показывает
Community navigation только после получения этих capabilities.

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
`DATABASE_URL`. E2E использует три изолированных browser contexts и проверяет
create → preview → join → role update → rotate/revoke → remove.

## Security и observability

- API различает `CommunitySpaceId` и `SyncSpaceId`.
- Forbidden cross-Space reads маскируются как not found.
- В БД хранится SHA-256 token hash; replay create/rotate использует
  purpose-bound encrypted `SecretStore`.
- Raw token не попадает в idempotency response storage, sync changes, activity
  и application logs.
- Remove/revoke проверяются на каждом server read; browser cache не является
  authorization source.
- Rate limit возвращает `429`, invalid mutation — `422`.

## Отложенные зависимости

Scope, зависящий от contracts `0.4.0`, зафиксирован в
[`../tmp-plans/0.5.0-deferred-until-0.4.0.md`](../tmp-plans/0.5.0-deferred-until-0.4.0.md).
