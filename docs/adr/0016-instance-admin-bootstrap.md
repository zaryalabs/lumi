# ADR 0016: Bootstrap администратора экземпляра через auth lookup id

Status: accepted

## Контекст

Lumi уже использует seed-derived challenge auth, durable web sessions и
стабильный `user_id`. Встроенный Telegram-бот относится ко всему экземпляру,
но его токен временно мог изменить любой авторизованный пользователь.

Передача recovery phrase серверу или в web UI как признака администратора
нарушила бы существующую auth boundary: эта фраза позволяет получить signing
key и должна оставаться только у пользователя.

## Решение

- Вводится instance-wide роль `user | admin`. Она не связана с ролями personal
  или shared sync spaces.
- Deployment задаёт один или несколько публичных account lookup id через
  `LUMI_ADMIN_LOOKUP_IDS`, разделённых запятыми.
- Lookup id выводится из recovery phrase локальной командой
  `make admin-lookup-id`. Команда читает phrase только через stdin и печатает
  только публичное значение.
- Сервер сопоставляет настроенные lookup id с активными `auth_identities` и
  вычисляет роль при проверке каждой web session.
- `/account/me` и auth bootstrap возвращают вычисленную `instance_role` для
  навигации и presentation logic.
- Все операции `/api/v1/settings/telegram*`, включая чтение, требуют `admin`.
  UI скрывает системный раздел от обычного пользователя, но authoritative
  проверка всегда выполняется сервером.
- Отсутствующая переменная означает отсутствие администраторов. Неверно
  закодированное значение блокирует запуск server до исправления конфигурации.

## Последствия

Первый срез не требует миграции PostgreSQL: `auth_identities.lookup_id` уже
является уникальным публичным идентификатором. Ротация deployment config
меняет права после перезапуска без переиздания cookie или изменения аккаунта.

Проверка роли добавляет один bounded lookup активных auth identities к
проверке сессии администратора или обычного пользователя.

## Альтернативы

- Raw recovery phrase в env отклонена: server compromise дал бы signing key и
  полный credential аккаунта.
- Проверка phrase или роли только в UI отклонена: прямой API request обошёл бы
  ограничение.
- `sync_space_members.role` отклонена: это scoped membership, а не право на
  конфигурацию экземпляра.
- Полноценный RBAC policy engine отложен до появления управляемых ролей,
  организаций или большого набора permissions.

## Compatibility

- Старые sessions продолжают работать; роль вычисляется при каждом request.
- Клиенты должны принимать новое поле `AccountSummary.instance_role`.
- API tests фиксируют `401` без session, `403` для `user` и доступ для `admin`.
- Web UI не должен отображать system settings при `instance_role = user`.
