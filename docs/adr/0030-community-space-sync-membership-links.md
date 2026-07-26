# ADR 0030: Community Space, sync projection и link access

Status: accepted

## Контекст

Первому social-срезу нужен закрытый Community Space, который два аккаунта
могут создать и открыть по отзывной ссылке. В репозитории уже существует
`sync_spaces`, но это infrastructure namespace для delivery, а не продуктовая
страница сообщества. Использование personal `space_id` как social ownership
root смешало бы ACL личного vault и общего контента.

Нужны одновременно:

- отдельная product identity Community Space;
- единая транзакция product state, membership projection и `sync_changes`;
- safe preview без неявного membership;
- роли owner/admin/member и server-authoritative permission matrix;
- 256-bit link token без plaintext в логах и базе;
- idempotent create/rotate/join с детерминированным ответом.

## Решение

`community_spaces.community_space_id` является product identity.
`community_spaces.sync_space_id` указывает на ровно один
`sync_spaces(kind = community)`. Они не взаимозаменяемы в API или domain.

Authoritative membership хранится в `community_memberships`. Таблица
`sync_space_members` является delivery projection и обновляется в той же
транзакции. Состояния:

```text
active -> left -> active
active -> removed
```

`removed` не возвращается в active общей ссылкой. Передача ownership будет
отдельным command; обычный role update не может назначить/понизить owner.

Каждая successful mutation:

1. проверяет active membership и role через общий pure policy;
2. проверяет `Idempotency-Key` и expected revision, где применимо;
3. изменяет authoritative community table;
4. обновляет `sync_space_members`/`sync_spaces`, если затронут delivery ACL;
5. добавляет `sync_changes` с schema `community-space.v1`;
6. добавляет payload-free activity event для системного действия;
7. сохраняет bounded response для deterministic retry;
8. commit-ит транзакцию целиком.

Forbidden cross-Space detail/list возвращается как not found, чтобы не
подтверждать существование объекта. Preview является public bounded route и
возвращает только name, description и member count.

Access token содержит 32 случайных байта и передаётся в fragment URL. В
`community_access_links` хранится SHA-256 hash для lookup. Чтобы повтор запроса
create/rotate с тем же idempotency key мог вернуть тот же one-time token без
plaintext storage, token хранится в account/purpose-bound encrypted
`SecretStore` envelope. Raw token не входит в `idempotency_keys`,
`sync_changes`, activity или logs.

## Permission matrix

| Действие | Owner | Admin | Member |
| --- | ---: | ---: | ---: |
| Читать Space | да | да | да |
| Менять identity/settings | да | да | нет |
| Управлять ссылками | да | да | нет |
| Назначать admin | да | нет | нет |
| Удалять участника | да | только member | нет |
| Удалять Space | да | нет | нет |

Неактивный membership не имеет прав независимо от сохранённого cursor или
действующей ссылки.

## Последствия

- Product ACL не зависит от personal vault и не может случайно открыть личные
  материалы.
- Новый участник получает bootstrap/tail через community SyncSpace без
  превращения change feed в source of truth.
- Revoke/remove действует на следующем server read немедленно; client cache не
  является authorization source.
- SecretStore становится частью availability boundary link create/rotate
  replay, но preview/join используют только token hash.
- Avatar/cover, fingerprints, shared anchors и social search не получают
  placeholder implementations до готовности их prerequisites.

## Compatibility

Forward-only migration `20260726260000_community_spaces_access.sql` добавляет
новые таблицы и валидирует существующие значения `sync_spaces.kind` и
`sync_space_members.role`. Personal SyncSpace сохраняет прежнюю семантику.

Контракт публикуется capabilities `community-spaces` и
`community-link-access`. Остальные social capabilities остаются выключенными,
пока соответствующие end-to-end verticals не готовы.
