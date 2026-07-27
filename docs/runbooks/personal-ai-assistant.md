# Персональный AI-ассистент

Status: `accepted`

Этот runbook описывает production path эпика `0.2.0/E1`: explicit source
context, OpenRouter BYOK, глобальный durable chat и Reader citations.

## Конфигурация

По умолчанию server обращается к
`https://openrouter.ai/api/v1/chat/completions`. Endpoint можно заменить только
instance-level переменной:

```sh
LUMI_OPENROUTER_ENDPOINT=https://openrouter.ai/api/v1/chat/completions
```

Публичный endpoint обязан использовать HTTPS. HTTP разрешён только для
loopback mock в тестах. Account credential вводится в Web-настройках
OpenRouter, проверяется до записи и хранится в `SecretStore`; API reads
возвращают лишь state и короткий keyed fingerprint.

## Пользовательский flow

1. Открыть «ИИ-чат» и настройки OpenRouter.
2. Выбрать разрешённую модель, вставить ключ и нажать «Проверить и сохранить».
3. Создать разговор вручную либо выделить текст в reflowable/PDF Reader.
4. Для выделения выбрать «Спросить ИИ», «Объясни проще» или «Кратко
   перескажи». Только выбранный source scope попадёт в context pack.
5. Кнопка «Источник» в ответе открывает исходный material/revision/anchor.

Разговоры, сообщения, частичный provider output, terminal state, usage и SSE
events записываются в PostgreSQL. Reload читает durable state; SSE reconnect
передаёт `Last-Event-ID` и получает только последующие события.

## Безопасность и отказ

- Ключ не сохраняется в browser storage и не возвращается через API.
- Resolver одним owner-scoped SQL read проверяет material, active revision и
  permission boundary до загрузки текста.
- Context ограничен frozen limits; stale revision/anchor и PDF без text layer
  завершаются безопасной ошибкой до provider call.
- Provider response и diagnostics не содержат credential или upstream body.
- После restart незавершённая generation переводится в `failed` с кодом
  `server_restarted`; пользователь может выполнить retry.
- Удаление ключа атомарно удаляет credential row и secret envelope.

## Проверка

```sh
make c
make pg-t
make web-e2e
```

Playwright запускает локальный OpenRouter-compatible mock. Для ручной проверки
настоящего provider использовать личный тестовый ключ и после smoke удалить
его через UI.

Диагностика:

- `missing_credential` — добавить или повторно проверить ключ;
- `authentication` — OpenRouter отклонил ключ;
- `rate_limited`, `timeout`, `unavailable` — повторить generation;
- `source_selection_is_stale` — заново выбрать текст в текущей revision;
- `source_has_no_usable_text_layer` — для PDF требуется text layer.

Нельзя логировать request Authorization, plaintext credential, provider body
или полный context pack.
