# ADR 0012: Встроенный Telegram-бот с настройкой через UI

Status: accepted

## Контекст

Telegram transport был разделён между local long polling в отдельном процессе
и production webhook. API и UI при этом могли объявлять Telegram доступным без
реального bot token. Для текущего прототипа это создаёт лишние режимы запуска и
не даёт пользователю настроить интеграцию из Lumi.

## Решение

- Один Telegram-бот относится ко всему экземпляру Lumi.
- Любой авторизованный пользователь временно может установить, заменить или
  удалить токен. Это сознательное прототипное ограничение до появления ролей.
- Токен проверяется через Telegram `getMe`, хранится как AEAD-шифротекст и
  никогда не возвращается клиенту или в logs.
- Локальный master key хранится в отдельном persistent secret root с правами
  `0600`; он не хранится в PostgreSQL.
- `lumi-server` запускает long polling supervisor поверх типизированного
  `teloxide-core` клиента. Замена токена перезапускает listener без перезапуска
  сервера.
- Bot scope выводится из стабильного Telegram bot id:
  `telegram-bot:<bot_id>`. Ротация токена того же бота сохраняет привязки.
- После успешного `getMe` active listener публикует Bot API client в
  late-bound media registry из ADR 0013. Import worker видит только capture
  interface и исходный `bot_id`, но не token, encryption key или download URL.
- Webhook, отдельный runner и Telegram env-конфигурация удаляются из активного
  runtime.

## Последствия

Обычного `make up` достаточно для полного backend lifecycle. Без токена Lumi
работает, а Telegram остаётся в состоянии `unconfigured`. Потеря локального
master key не раскрывает токен, но требует ввести его заново после restore.

Настройка содержит `configured_by_user_id`, поэтому будущая проверка роли может
быть добавлена на существующую API-границу без изменения UI-контракта.

## Отложенный webhook transport

Webhook остаётся допустимым будущим вариантом для нескольких server replicas и
большой нагрузки. Он должен быть отдельным transport adapter над тем же
transport-neutral `TelegramService`, использовать `setWebhook` с
`secret_token`, быстро фиксировать durable update и не выполнять импорт внутри
HTTP request. Возврат к webhook требует отдельного ADR после появления ролей,
публичного HTTPS deployment и production secret manager.
