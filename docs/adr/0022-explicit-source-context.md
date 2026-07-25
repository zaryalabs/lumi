# ADR 0022: explicit source context и citations

Status: accepted

## Контекст

`0.2.0` должен поддержать selection/chapter/material AI context до появления
Tantivy, fastText и library-wide retrieval. Контекст должен быть
детерминированным, ограниченным, revision-bound и одинаковым для chat,
внутреннего worker и MCP agent. Permission snapshot не должен превращаться в
долгоживущий capability.

## Решение

- `SourceContextResolver` принимает authenticated actor и exact
  `SourceScope`: `selection`, `chapter` или `material`, обязательно с
  `material_id` и `revision_id`; selection дополнительно содержит общий
  source-backed `Anchor`.
- Resolver читает только Normalized Content Package/text layer указанной
  revision. Он не выполняет поиск, ranking или implicit library expansion.
- `AiContextPack` immutable и versioned. Он содержит ordered fragments,
  `SourceCitation`, policy/limit version, normalized content hashes,
  permission snapshot metadata и итоговый hash pack.
- Fragment содержит stable unit/block или PDF page/text-block identity,
  bounded UTF-8 text и label. Инструкции из source обрабатываются как
  недоверенные quoted data и не могут менять system policy, tools или scope.
- `SourceCitation` содержит opaque citation id, material/revision, unit/block
  либо PDF page, общий Anchor/source locator, quote hash и fragment range.
  Ответ/artifact ссылается на citation ids только из своего context pack.
- Порядок material scope следует reading order/spine; chapter ограничивается
  одним content unit; selection начинает с exact quote и добавляет только
  bounded соседние blocks той же revision.
- Default profiles: selection — 24 KiB/16 fragments, chapter — 64 KiB/64
  fragments, material — 128 KiB/128 fragments. Hard ceiling одного pack —
  256 KiB UTF-8; один fragment не больше 16 KiB. Provider token budget может
  уменьшить, но не увеличить hard limits.
- Oversize дает явную truncation diagnostic и continuation cursor/section
  plan. Большая книга обрабатывается иерархически; она не отправляется одним
  request.
- Permission проверяется при build, при выдаче pack MCP/worker и перед
  publication. Snapshot фиксирует принятое решение для audit, но не отменяет
  повторную авторизацию. Stale/missing revision и отсутствующий PDF text layer
  возвращают typed errors.
- Persisted pack хранит фактически отправленные fragments для
  reproducibility и наследует account content retention. Provider credential,
  raw auth data и несвязанные annotations в pack не входят.

## Последствия

- `CORE-013` реализуем без зависимости от `SEARCH-001`–`SEARCH-005`.
- EPUB, Markdown, `.lum` и PDF используют один citation DTO поверх разных
  source locators.
- Chat attachment preview может точно показать, что покинет Lumi account.
- Indexed retrieval позже должен возвращать те же fragment/citation contracts,
  но остается отдельной capability.

## Альтернативы

- Отправлять целый material или библиотеку: отклонено по privacy/cost limits.
- Хранить только plain quote без revision/anchor: отклонено; переход к source и
  stale detection невозможны.
- Считать permission snapshot bearer capability: отклонено; отзыв доступа не
  должен обходиться старым pack.
- Временно встроить linear scan в search API: отклонено; это смешивает explicit
  scope и ranked retrieval.

## Совместимость и проверки

- Contract markers: `ai-context-pack.v1`, `source-citation.v1`,
  `explicit-context-limits.v1`.
- Golden fixtures покрывают EPUB, Markdown, PDF page geometry, `.lum`,
  кириллицу/emoji, stale revision, missing text layer, oversize и
  cross-account ids.
- Исполняемый probe находится в `spikes/stage0/src/explicit_context.rs`.
