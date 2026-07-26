# Внешние агенты через MCP

Status: `active`

## Назначение

Lumi `0.2.0/E3` предоставляет account-scoped MCP Streamable HTTP endpoint
`POST /mcp`. Подключение действует с правами обычного пользователя одного
аккаунта и не открывает provider credentials, административные операции,
удаление аккаунта или внутренний global chat.

## Создание подключения

1. Откройте `Подключения` → `MCP`.
2. Введите имя клиента и нажмите `Создать подключение`.
3. Скопируйте endpoint и bearer token из одноразового сообщения.
4. Передайте token только доверенному MCP-клиенту. Lumi хранит verifier, а
   plaintext повторно не показывает.

Пример конфигурации клиента:

```json
{
  "mcpServers": {
    "lumi": {
      "url": "https://lumi.example/mcp",
      "headers": {
        "Authorization": "Bearer lumi_mcp_REDACTED"
      }
    }
  }
}
```

Клиент начинает с `initialize`, затем вызывает `tools/list`.
`MCP-Protocol-Version` для последующих запросов — `2025-06-18`.

## Доступный срез

- capability discovery;
- list/get/TOC/paginated чтение и bounded download материала;
- URL и bounded text import, import status;
- archive/restore и двухшаговое permanent delete;
- list/create/update/delete annotations;
- создание summary task и чтение summary/artifact;
- list/claim/context/progress/complete/fail/release AI tasks.

`tools/list` публикует только операции, реально доступные на этом экземпляре.
Нереализованные search, bookmark, abridgement и large upload-ref tools не
имитируются.

## Ротация и отзыв

`Ротировать токен` немедленно делает прежний token недействительным и один раз
показывает новый. `Отозвать` блокирует следующий запрос подключения. Уже
выданный task claim всё равно обязан пройти `run_id + claim_id + fence +
task_revision`; stale completion не публикует artifact.

При подозрении на утечку сначала отзовите connection в Web, затем проверьте
очередь AI-задач и повторите незавершённые задачи новым подключением.

## Smoke

```sh
curl -sS https://lumi.example/mcp \
  -H 'Authorization: Bearer lumi_mcp_REDACTED' \
  -H 'Content-Type: application/json' \
  -H 'MCP-Protocol-Version: 2025-06-18' \
  --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}'
```

После revoke тот же запрос должен вернуть `401` и
`WWW-Authenticate: Bearer realm="lumi-mcp"`.

