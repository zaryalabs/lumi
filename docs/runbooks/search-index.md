# Search index и fastText

Status: accepted

Runbook относится к `0.4.0/E3` и ADR
[`0033`](../adr/0033-search-chunks-tantivy-fasttext.md).

После `0.4.0/E5` актуальные derived versions — `tantivy.v2` и
`search.chunker.v2`: chunk payload дополнительно хранит lifecycle/update
metadata для record scopes. Обновление с v1 требует full rebuild.

## Конфигурация

Search primary data остаётся в PostgreSQL и normalized packages. Tantivy
directory задаётся:

```sh
export LUMI_SEARCH_ROOT=/var/lib/lumi/search
```

Обычный query profile требует fastText model:

```sh
export LUMI_FASTTEXT_MODEL=/var/lib/lumi/models/cc.ru.300.bin
export LUMI_FASTTEXT_MODEL_VERSION=cc.ru.300.fasttext.v1
export LUMI_FASTTEXT_MODEL_SHA256="$(sha256sum "$LUMI_FASTTEXT_MODEL" | cut -d' ' -f1)"
```

На macOS вместо `sha256sum` используется `shasum -a 256`. Deployment manifest
должен хранить ожидаемый checksum независимо от вычисленного на узле значения;
команда выше подходит только для первичной операторской сверки.

Поддерживаются standard fastText `.bin` и finalfusion `.fifu`. Для production
предпочтителен заранее проверенный quantized `.fifu`, если он проходит тот же
golden corpus. Upstream Common Crawl vectors распространяются отдельно по
CC BY-SA 3.0; Lumi не включает model bytes в image/repository.

Без path/checksum, при mismatch или invalid bytes сервер остаётся доступным, но
`GET /api/v1/search/status` возвращает `failed`, а capabilities не публикуют
`search-query`/`ai-retrieval`.

## Проверка состояния

Авторизованный пользователь:

```sh
curl -sS -b cookies.txt http://127.0.0.1:8080/api/v1/search/status
```

Состояния:

- `ready` — index/model готовы, pending jobs отсутствуют;
- `partial` — часть projection доступна, очередь ещё не догнана;
- `rebuilding` — выполняется полный account rebuild;
- `failed` — query отключён; `failure_code` безопасен для logs/tickets.

`no_text_document_count` отдельно показывает active sources без индексируемого
текста (например, Voice Note до принятого transcript); это не failure и не
причина скрывать уже готовый index.

Тексты query/chunks не пишутся в обычную telemetry. Для диагностики достаточно
job id, owner id, stage, queue depth, generation и failure code.

## Полный rebuild

Пользовательский authorized rebuild:

```sh
curl -sS -X POST -b cookies.txt \
  -H "x-lumi-csrf: $LUMI_CSRF" \
  http://127.0.0.1:8080/api/v1/search/rebuild
```

Команда ставит общий fenced `search_index` job. Primary materials, annotations,
AI artifacts и learning items не изменяются. Повтор безопасен: worker удаляет
только owner-scoped derived Tantivy/PostgreSQL projection.

Rebuild обязателен после:

- изменения Tantivy schema/index version;
- изменения chunker version;
- замены fastText model/version;
- восстановления PostgreSQL/packages без search volume;
- `search_index_schema_mismatch` или подтверждённого повреждения index.

## Застрявшие jobs

Common `JobRuntime` автоматически возвращает expired claim в очередь, пока не
исчерпан `max_attempts`. Проверять нужно только payload-free metadata:

```sql
SELECT job_id, user_id, status, stage, attempt, max_attempts,
       lease_expires_at, error_code
FROM jobs
WHERE kind = 'search_index'
ORDER BY created_at DESC
LIMIT 100;
```

После исчерпания попыток account state становится `failed`. Сначала устраните
model/disk/PostgreSQL причину, затем запросите full rebuild; не меняйте status
строки вручную.

## Rollback

Остановка search worker или удаление search volume не блокирует Reader,
annotations, learning или AI explicit-context paths. Для rollback:

1. остановить process;
2. сохранить PostgreSQL и normalized/blob data;
3. удалить только versioned directory под `LUMI_SEARCH_ROOT`;
4. вернуть совместимую binary/model config;
5. запустить process и full rebuild;
6. дождаться `ready` и сверить golden queries.

`search_documents`, `search_chunks` и vectors также derived; их можно
перестроить. Таблицы primary domain и normalized packages удалять нельзя.
