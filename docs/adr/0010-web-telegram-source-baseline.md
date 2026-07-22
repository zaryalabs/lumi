# ADR 0010: Общий publication path для Web и Telegram

Status: accepted

## Контекст

EPUB уже публиковал `DocumentRevision`, Normalized Content Package и
`ReadingDocument` через durable job. Web-страницы и Telegram-сообщения не
должны создавать отдельные library/reader models, но требуют другого
provenance, сетевой границы и provider idempotency.

## Решение

- Все источники проходят `source adapter -> ImportedPublication ->` общий
  атомарный publication path. Persisted formats: `epub`, `web_page`,
  `telegram`; одиночная Telegram URL только маршрутизируется в `web_page`.
- Package/manifest ids детерминированы внутри `revision_id`. Normalized package
  version не меняется: tagged Web/Telegram locators и external links additive.
  Domain marker повышен до `s1.2026-07-13.sources-v2`, каталог — до `s1-0006`.
- Web baseline — bounded raw HTTP fetch. Immutable snapshot сохраняет
  submitted/final/canonical URL, redirects, status, charset, capture provider,
  raw HTML и checksums. Cloud browser rendering отложен за тем же contract.
- Все DNS answers и каждый redirect hop проверяются до соединения; client
  pin-ится к проверенному IP и не использует proxy, cookies или subresources.
  Допускается global unicast с консервативными IANA special-range исключениями.
- Extractor не рендерит исходный HTML, удаляет boilerplate, ограничивает
  blocks/links/total text/package bytes и сохраняет source-backed locators.
- Pairing token случайный, хранится как domain-separated hash, действует 10
  минут и потребляется один раз. Pairing update claim, consume/link и outcome
  фиксируются одной PostgreSQL transaction.
- `(bot_scope, update_id)` связан с payload hash и durable outcome. Duplicate
  возвращает тот же ответ; другой payload конфликтует. Первоначальный отдельный
  local runner заменён встроенным `teloxide-core` listener и UI settings в
  [ADR 0012](0012-embedded-telegram-bot-settings.md). API server выполняет один
  lease-safe recovery: активный claim остаётся нетронутым, а expired job
  атомарно переочередивается и может быть захвачен лишь один раз.
- Admission сериализован до blob write: максимум 16 незавершённых imports на
  аккаунт и 8 активных workers. Durable worker claim/lease запрещает stale
  process публиковать после recovery другим process.

## Последствия

- Web/Telegram используют общие library, reader, progress и annotations.
- Raw fetch честно не поддерживает JS-only pages.
- Консервативная network policy может отвергнуть редкий special-use endpoint.
- Telegram text/forward attribution — личный cloud content; UI сообщает это до
  pairing, source download не кэшируется.

## Отклонённые альтернативы

- Persisted DOM path без semantic locator; automatic redirects; повторное
  использование consumed token; recovery без claim/lease fencing; отдельная
  Telegram library — все варианты нарушают стабильность, безопасность или
  общий model.

## Совместимость и проверки

- Migration `20260714020000_stage6_web_telegram_sources.sql` добавляет source
  kind, worker lease, pairing identities/tokens и update log; legacy jobs
  backfill-ятся как EPUB, legacy EPUB source ref остаётся читаемым.
- Committed Web HTML и Telegram JSON fixtures покрывают semantics, forwarded
  provenance, плохую extraction и browser URL journey.
- SSRF corpus сверяется с [IANA IPv4](https://www.iana.org/assignments/iana-ipv4-special-registry/iana-ipv4-special-registry.xhtml)
  и [IANA IPv6](https://www.iana.org/assignments/iana-ipv6-special-registry/iana-ipv6-special-registry.xhtml)
  special-purpose registries.
- Обязательны tests pairing expiry/single-use/unlink, duplicate/conflict,
  cancellation, stale worker claim и idempotent admission.
