# RAG по личным записям

Status: accepted

Runbook относится к `0.4.0/E5` и
[`ADR 0035`](../adr/0035-record-scoped-rag-global-chat.md).

## Пользовательский flow

1. В Search, Desk или Reader выбрать «Спросить по записям».
2. До отправки проверить видимый material/type/tag/status scope.
3. Ввести вопрос в существующий глобальный AI-чат.
4. Lumi выполняет owner-filtered retrieval, показывает включённые records и
   только затем вызывает настроенный OpenRouter.
5. Citation открывает исходный Desk item либо Reader anchor.

Нужны готовые capabilities `search-query`, `ai-global-chat` и `record-rag`, а
также валидный account BYOK. Отсутствие ключа ведёт в настройки provider и не
включает скрытый fallback.

## Диагностика

Без чтения пользовательского текста проверять:

- search state/generation, queue depth и failure code;
- generation id/status, provider/model, token usage и normalized error;
- количество retrieval chunks, число разных sources и pack/prompt versions;
- наличие `record_scope`, chunk ids/hashes и citation ids в immutable pack.

Обычные коды:

- `record context scope is invalid` — client прислал неподдерживаемый или
  oversized scope;
- `record context is too weak to ground an answer` — уточнить вопрос/filters
  или дождаться index update;
- `record retrieval` unavailable — проверить fastText checksum, Tantivy volume
  и `/api/v1/search/status`;
- provider errors — действовать по
  [`personal-ai-assistant.md`](personal-ai-assistant.md).

Query, note body, exact context и provider response в обычные logs не пишутся.

## Rebuild и восстановление

После обновления на `search.chunker.v2`/`tantivy.v2` выполнить account rebuild
по [`search-index.md`](search-index.md). Primary annotations, links, learning,
artifacts и chat history не удаляются. После cold start дождаться `ready`,
затем проверить record query и citation open target.

Backup включает PostgreSQL `ai_context_packs`, messages/generations и primary
records; Tantivy остаётся rebuildable derived data.

## Отключение и rollback

Чтобы временно отключить RAG, убрать/исправить search model configuration либо
отключить provider delivery. Capability `record-rag` исчезнет, обычные
Search/Desk/Reader данные сохранятся. Для rollback binary:

1. сохранить PostgreSQL и blob data;
2. остановить server;
3. вернуть совместимую binary;
4. удалить только versioned `tantivy.v2` derived directory при необходимости;
5. запустить server и перестроить совместимый index.

JSON additions не требуют schema contraction; старый source-chat path
продолжает читать прежние messages/packs.

## Проверка

```sh
make c
make pg-t
make security
make search-performance
make web-e2e
```

Deterministic tests не обращаются к live provider. Особенно проверить foreign
account scope, malicious note instructions, empty/weak retrieval, reload,
cancel/retry и citation → Desk/Reader navigation.
