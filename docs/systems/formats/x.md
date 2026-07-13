# X: длинные посты и треды

Status: accepted

## Контекст

X является отдельным источником для Lumi, не вариантом обычного web-reader
импорта. На X текст, треды, длинные посты, Articles, replies, quotes, edits,
deletions, protected content и media живут в platform-specific модели.
Production/compliance path должен идти через официальный X API. Дополнительный
user-initiated fallback через browser extension допускается для контента, который
пользователь уже видит в своем браузере, но сохраняется как extension snapshot с
degraded compliance metadata, а не как crawler/scraper path.

Главное решение: X-реализация - это `XImportProvider`, который получает Post
данные через официальный API, нормализует одиночные posts, треды и long-form
content в `DocumentRevision` and Normalized Content Package, сохраняет source
map и compliance metadata.
Reader остается общим: он отвечает за типографику, anchors, заметки, поиск,
обучение, ИИ-действия и социальные слои.

Browser extension может создать `XExtensionSnapshot` для видимого post/thread
selection. Такой snapshot проходит через тот же normalizer where possible and
must preserve capture provider/version, visible URL, captured_at, detected post
ids, author handles and lower-confidence compliance state.

Поддерживаем не только треды. Для `v01` направление X должно покрывать три
материальных формы:

- одиночный X Post;
- авторский thread, собранный из последовательности posts;
- long-form content: long post через `note_tweet` и X Article через `article`
  payload, если он доступен через стандартный Post Lookup API.

## Пользовательские сценарии

- Пользователь вставляет ссылку на X Post.
- Lumi импортирует одиночный post с автором, датой, media, ссылками и quoted
  context.
- Пользователь вставляет ссылку на один post из треда, а Lumi собирает
  авторский thread в правильном порядке.
- Пользователь сохраняет длинный X post и читает полный текст, а не только
  короткий `text` preview.
- Пользователь сохраняет X Article/лонгрид, если post ссылается на article
  payload, доступный через API.
- Пользователь читает X-материал в том же visual reader, что веб-статьи, EPUB,
  FB2, Markdown и `lum`.
- Пользователь делает хайлайты, заметки, записи на полях и voice notes с
  устойчивой привязкой к post/thread/article фрагментам.
- Пользователь ищет по сохраненным X-материалам и использует их как контекст
  для базы знаний, обучения и ИИ-действий.
- Пользователь видит, если тред импортирован частично из-за API-доступа, лимитов
  или недоступности старого conversation search.
- Пользователь может открыть исходный post/thread/article на X, чтобы проверить
  оригинальный контекст.

## Функциональные требования

### Поддерживаемый X content

- Public X Posts по URL.
- Public author threads: цепочка self-replies автора, а не вся ветка
  обсуждения.
- Long posts через поле `note_tweet`, если API возвращает полный long-form
  текст.
- X Articles через поле `article` и связанные `article.*` expansions, если API
  возвращает article content для post.
- Quoted posts как вложенный context block.
- Media attachments: images, GIF/video metadata, alt text, preview images и
  variants как resources/placeholders.
- Polls как статический poll block с состоянием на момент импорта.
- Links, hashtags, mentions, cashtags и parsed entities.
- Edit history metadata через `edit_history_tweet_ids`.

Не входит в основной `v01` path:

- home timeline;
- arbitrary user timeline import;
- X search/discovery как продуктовая лента;
- Direct Messages;
- автоматические replies, likes, reposts или любые write actions;
- protected content для пользователей, которые не имеют права его видеть;
- scraping, browser automation и HTML extraction X pages.

### API access model

Основной путь для `v01`:

- официальный X API v2;
- app-only bearer token для public read сценариев, где endpoint это позволяет;
- OAuth 2.0 PKCE user context только для сценариев, которым нужен доступ к
  пользовательским данным: bookmarks, liked posts, protected content или owned
  data.

Для первой реализации достаточно URL import через official Post Lookup API.
OAuth/bookmarks стоит проектировать как следующий слой, чтобы не смешивать
базовый reader import с account integration, consent, refresh tokens и privacy
policy.

Lumi должен поддерживать BYO credentials mode для self-hosted/open-source
использования: пользователь или инстанс задает собственный X developer app и
spending limits. Hosted Lumi может иметь собственный app, но тогда нужен
централизованный cost control.

### Browser extension snapshot fallback

Browser extension может сохранить X content, который пользователь явно выбрал в
открытой вкладке:

- одиночный видимый post;
- видимую часть thread;
- selected text/media metadata;
- canonical/current URL, detected post ids, author handles and timestamps.

Ограничения:

- extension не становится crawler-ом и не обходит access restrictions;
- cookies/tokens не передаются Lumi как secrets;
- snapshot не считается равноценным official API hydration;
- thread может быть partial and must show visible diagnostics;
- compliance recheck через official API выполняется later, если доступен;
- sharing/export должны учитывать degraded compliance state.

### URL import

Importer должен распознавать:

- `https://x.com/{username}/status/{post_id}`;
- `https://twitter.com/{username}/status/{post_id}`;
- mobile/share URL variants с query parameters;
- canonical source URL после нормализации.

URL parsing не должен доверять username как идентификатору автора. Primary key -
numeric `post_id`. Username сохраняется как hint и используется только для
диагностики или display fallback.

### Post lookup fields

Для одиночного post importer запрашивает минимальный расширенный набор полей:

- `tweet.fields`: `id`, `text`, `note_tweet`, `article`, `author_id`,
  `created_at`, `conversation_id`, `in_reply_to_user_id`, `referenced_tweets`,
  `attachments`, `entities`, `display_text_range`, `edit_history_tweet_ids`,
  `lang`, `possibly_sensitive`, `public_metrics`, `reply_settings`, `withheld`.
- `expansions`: `author_id`, `attachments.media_keys`,
  `attachments.poll_ids`, `referenced_tweets.id`,
  `referenced_tweets.id.author_id`,
  `referenced_tweets.id.attachments.media_keys`, `edit_history_tweet_ids`,
  `article.cover_media`, `article.media_entities`,
  `entities.mentions.username`.
- `media.fields`: `media_key`, `type`, `url`, `preview_image_url`, `width`,
  `height`, `alt_text`, `duration_ms`, `variants`.
- `poll.fields`: `id`, `options`, `duration_minutes`, `end_datetime`,
  `voting_status`.
- `user.fields`: `id`, `username`, `name`, `verified`, `verified_type`,
  `profile_image_url`, `protected`, `withheld`.

Поля должны быть централизованы в одном API profile, чтобы стоимость, лимиты и
совместимость можно было менять без переписывания importer.

### Одиночный post

Одиночный post нормализуется в Normalized Content Package, из которого
строится компактный `ReadingDocument`:

- title: author display name + краткий текстовый preview;
- metadata: post URL, author, username, created date, lang, metrics snapshot;
- body: post text или `note_tweet` text, если он есть;
- inline entities: links, mentions, hashtags, cashtags;
- media figures;
- poll block;
- quoted post context;
- reply context как optional metadata, а не обязательная часть документа.

Если post является reply, importer не должен автоматически импортировать всю
conversation. Он может добавить source context: replied-to post id, author hint
и link to original. Полная ветка обсуждения не является материалом для чтения
по умолчанию.

### Thread import

X API не дает отдельного "get thread" endpoint. Thread нужно реконструировать
из post graph.

Определение thread для Lumi:

- авторский thread - это последовательность posts одного автора;
- posts связаны reply chain через `referenced_tweets.type = replied_to`;
- `conversation_id` указывает root conversation;
- replies других людей не входят в основной reading flow;
- quoted posts являются context blocks, но не становятся частью основного
  thread, если пользователь явно не импортирует quote separately.

Algorithm:

1. Получить исходный post по URL.
2. Получить `conversation_id`, `author_id`, `referenced_tweets` и `created_at`.
3. Если post не является root, найти root conversation post по
   `conversation_id`.
4. Получить candidate posts conversation через Search API:
   `conversation_id:<root_id>`.
5. Добавить root post, потому что search replies может не вернуть root.
6. Построить directed graph по `replied_to` references.
7. Отфильтровать same-author posts, связанные с root/current post.
8. Отсортировать deterministic order: graph path first, затем `created_at`,
   затем numeric post id.
9. Сохранить skipped replies других пользователей как optional conversation
   metadata/count, не как основной текст.
10. Создать один `ReadingDocument`, где каждый post становится section/block.

Ограничения:

- Recent Search покрывает только recent window.
- Для старых тредов нужен Full-Archive Search, если он доступен текущему API
  плану.
- Если Full-Archive недоступен, Lumi импортирует исходный post, referenced
  context и явно помечает thread as partial.
- Пользователь может вручную добавить дополнительные post URLs в тот же
  материал, если API не может найти старые части thread.

Thread reconstruction должен быть conservative. Лучше честно показать partial
thread, чем ошибочно смешать conversation replies и авторский материал.

### Long-form posts

Long-form post поддерживается через `note_tweet`.

Правила:

- Если `note_tweet` присутствует, primary text берется из него, а не из
  короткого `text`.
- `text` сохраняется как preview/source field.
- `display_text_range` используется как hint для отображаемой части короткого
  post, но не заменяет full text.
- Entities внутри long-form payload должны мапиться в links/mentions/hashtags,
  если API возвращает их отдельно.
- Anchor source хранит post id и range внутри normalized long-form text.

Long-form post для reader выглядит как обычная статья без глав: author header,
long text body, media/resources, quoted context и source metadata.

### X Articles / лонгриды

X Article поддерживается как отдельный content kind внутри X importer.

Правила:

- Если Post Lookup возвращает `article` payload, importer строит
  `ReadingDocument` из article title/body/entities и связанных media.
- Post, через который article опубликован, сохраняется как publication wrapper:
  source post id, author, published date, metrics и URL.
- `article.cover_media` и `article.media_entities` сохраняются как resources.
- Если API возвращает article metadata, но не возвращает полный body, importer
  создает partial material с понятным `ImportIssue` и source link.
- Article не импортируется через generic web-reader с `x.com` URL, потому что
  это нарушает выбранную API-first границу и compliance model.

Для reader X Article похож на web article: title, author, sections/paragraphs,
links, images, embeds/placeholders и source metadata. Отличие - source map
привязан к X post/article ids, а не к HTML DOM.

### Links и entities

- `entities.urls` превращаются в external reader links с expanded URL и display
  URL.
- `t.co` URL не используется как final source, если API дает `expanded_url`.
- Mentions превращаются в links на X profile, но не импортируют profile
  автоматически.
- Hashtags и cashtags сохраняются как semantic inline marks и clickable source
  links.
- Links на другие X posts могут стать import actions, но не импортируются
  автоматически, кроме quoted/referenced posts, запрошенных через expansions.
- Links на обычные статьи могут запускать отдельный web-reader import, если
  пользователь выбирает "import linked article".

### Media

- Images сохраняются как локальные resources материала, если API возвращает
  direct image URL и policy позволяет хранение.
- `alt_text`, dimensions, media key и source URL сохраняются.
- GIF/video сохраняются как media placeholder: preview image, variants metadata,
  duration, source URL. Автозагрузка видео не является обязательной для `v01`.
- Sensitive media помечается флагом `possibly_sensitive` и отображается через
  reader policy.
- Media, недоступные через API или заблокированные storage policy, заменяются
  placeholder-ом и `ImportIssue`.

### Edits, deletions и compliance

X content не является обычным immutable snapshot без внешних обязательств.
Если Lumi хранит X content offline, нужна compliance-модель:

- сохранять `edit_history_tweet_ids`;
- сохранять `hydrated_at` и API response metadata;
- при повторном открытии или периодической sync rehydrate critical X content по
  политике freshness;
- если post удален, suspended, protected, withheld или стал недоступен, менять
  `XComplianceState`;
- если X или владелец content требует удаление, удалить/скрыть сохраненный X
  content в допустимый срок;
- не показывать protected content пользователям, которые не имеют права его
  видеть;
- при публичном sharing не redistributing hydrated content сверх policy limits.

Нужно отдельно решить, что происходит с личными заметками и хайлайтами, если X
content удален. Предварительное решение: пользовательские notes остаются, но
source quote может быть скрыт или заменен на tombstone в зависимости от
compliance policy.

### Ошибки и деградация

- Если post не найден, материал сохраняется как failed import с URL и причиной.
- Если post удален/protected/withheld, importer создает `ImportIssue` и
  compliance state вместо silent failure.
- Если thread частичный, reader показывает visible warning и список missing
  reasons: no full archive access, rate limited, deleted/protected posts,
  search window exceeded.
- Если long-form payload недоступен, importer не должен выдавать truncated text
  за полный material.
- Если API rate limit или credits закончились, importer ставит retryable status
  и не делает aggressive retries.
- Если media не загрузилось, текст остается читаемым.

## Нефункциональные требования

- **API-first with extension fallback.** Production/compliance importer
  использует официальный X API. User-initiated browser extension snapshot
  допустим как fallback, но scraping, crawler-style browser automation и generic
  HTML extraction X pages запрещены.
- **Единый вид.** X posts, threads и articles отображаются через общий
  reflowable reader contract, а не через embedded X UI.
- **Детерминированность.** Один API snapshot или extension snapshot при
  одинаковой версии importer должен давать одинаковые `ReadingNode` ids, source
  map и anchors where possible.
- **Cost control.** API calls должны быть bounded, кешироваться и учитывать
  pricing/rate limits. Нужны budgets, per-import estimates и retry policy.
- **Privacy.** OAuth tokens, user bookmarks и protected content не смешиваются с
  public import cache и не показываются другим пользователям.
- **Compliance.** Lumi должен уметь rehydrate, delete/tombstone и скрывать X
  content при изменении доступности.
- **Offline-first with caveat.** После импорта материал читается offline, но X
  content может требовать последующей compliance-синхронизации.
- **No AI training.** X content нельзя использовать для обучения моделей.
  Допустимы только user-initiated reader actions: объяснить, суммаризировать,
  сделать вопросы, связать с заметками.
- **Диагностируемость.** Importer сохраняет API profile, requested fields,
  response ids, import issues, partial thread reasons и adapter version.

## Модель данных

```text
XInput
  -> XAccessProvider
  -> XPostLookupSnapshot
  -> XThreadCandidate | XLongFormCandidate | XArticleCandidate
  -> XMediaResource[]
  -> DocumentRevision
  -> Normalized Content Package
  -> ReadingDocument
```

Формат-специфичные сущности:

- `XInput` - source URL, parsed post id, requested import mode:
  `single_post`, `thread`, `long_form`, `auto`.
- `XAccessProvider` - app-only или OAuth user-context API client.
- `XApiProfile` - централизованный набор fields/expansions и version.
- `XPostSnapshot` - raw normalized API object: id, text, note_tweet, article,
  author, entities, media, poll, metrics, edit history, hydrated_at.
- `XThreadCandidate` - root id, conversation id, candidate posts, graph edges,
  included/skipped posts, partial reasons.
- `XArticleCandidate` - article payload, wrapper post id, title, body, entities,
  media resources, partial state.
- `XMediaResource` - media key, type, local resource id, source URL, alt text,
  dimensions, variants.
- `XComplianceState` - `active`, `edited`, `deleted`, `protected`, `withheld`,
  `suspended`, `unknown`, `requires_recheck`.
- `XImportIssue` - warning/error с post id, API endpoint, reason и severity.

X-specific anchor source:

```text
XAnchorSource {
  source_url
  post_id
  conversation_id
  author_id
  content_kind
  thread_index
  article_id
  edit_history_tweet_ids
  text_offset_start
  text_offset_end
  hydrated_at
}
```

Primary anchor остается общей anchor-моделью Lumi: `ReadingNode` path, quote,
prefix/suffix context, content hash и `DocumentRevision`. X-specific source
нужен для восстановления, compliance recheck, экспорта и deep links.

## Реализация

### Pipeline импорта

1. Принять URL или direct post id.
2. Нормализовать URL и извлечь numeric post id.
3. Создать `Material` с source kind `x`.
4. Выбрать access provider: app-only или OAuth user context.
5. Выполнить Post Lookup с `XApiProfile`.
6. Определить content kind: single post, thread candidate, long-form post,
   article.
7. Если mode `auto`, выбрать:
   - `article`, если есть полноценный article payload;
   - `long_form`, если есть `note_tweet`;
   - `thread`, если post выглядит как часть author self-reply chain или
     пользователь запросил thread;
   - `single_post` иначе.
8. Для thread получить conversation candidates через Search API, если доступно.
9. Построить thread graph и отфильтровать author chain.
10. Нормализовать text/entities/media/polls/quotes в `ReadingNode`.
11. Сохранить media resources или placeholders.
12. Построить source map и X-specific anchor source.
13. Создать `DocumentRevision`, Normalized Content Package, `ReadingDocument`
    view, import issues и compliance state.
14. Передать normalized text в поиск и будущие ИИ/learning pipelines.

### Thread reconstruction details

Thread graph должен хранить больше данных, чем показывает reader:

```text
XThreadGraph {
  root_post_id
  conversation_id
  requested_post_id
  included_post_ids
  skipped_post_ids
  edges: replied_to[]
  partial_reasons[]
}
```

Это нужно, чтобы объяснять пользователю, почему тред выглядит именно так:
например, "часть треда старше recent search window" или "некоторые posts стали
недоступны".

### Выбор библиотек

Базовый Rust stack:

- `reqwest` - HTTP client для X API.
- `oauth2` - OAuth 2.0 PKCE flow для user-context features.
- `serde` / `serde_json` - typed API responses.
- `url` - URL parsing, canonicalization и entity link handling.
- `chrono` или `time` - timestamps, `created_at`, `hydrated_at`.
- `governor` или internal rate limiter - client-side rate/cost throttling.
- `twitter-text` equivalent или internal parser - optional validation/counting
  для entities, если понадобится совместимость с X text rules.

Отдельная SDK dependency не обязательна. Прямой typed client вокруг нужных
endpoints дает меньше surface area и проще контролирует fields, costs и
compliance.

### Cost и rate limit policy

- Все API calls идут через `XApiBudget`.
- На один импорт thread должен быть hard limit по количеству posts и страницам
  search pagination.
- Responses кешируются по post id и `hydrated_at`.
- Если один post уже hydrated в текущем 24-hour billing window, importer должен
  переиспользовать cache, где это совместимо с freshness/compliance.
- UI должен показывать, что full thread import может стоить дороже single post.
- Hosted Lumi должен иметь server-side spending limits.
- Self-hosted/BYO mode должен позволять пользователю задавать собственные API
  credentials и budgets.

### Security

- Не хранить API keys/tokens в коде или синхронизируемых plaintext settings.
- OAuth refresh tokens хранить только в secure local/server secret storage.
- Не логировать Authorization headers и raw tokens.
- Не отправлять X content во внешние AI providers без user-initiated action и
  настроек приватности.
- Не использовать X API keys других пользователей.
- Не пытаться обходить rate limits через несколько apps.

## Интеграции и зависимости

- **Reader.** X importer выдает Normalized Content Package; `ReadingDocument`
  является reader-facing view поверх него. Reader отвечает за paginated
  rendering, anchors, заметки, timeline events и панели.
- **Web reader.** X URLs не идут через generic web-reader. Web-reader может
  импортировать внешние links из X post только как отдельные user-initiated
  materials.
- **Поиск.** X importer передает normalized text, author, username, URL,
  hashtags, created date и source kind в индекс.
- **База знаний.** Заметки и хайлайты экспортируются с source: author, username,
  post/thread/article URL, created date, quote и backlink в Lumi.
- **Obsidian.** Экспорт X-заметок должен давать Markdown с canonical X URL,
  author attribution, retrieved/hydrated date и wikilinks.
- **ИИ.** Reader передает ИИ-слою normalized text и anchor context. X importer
  не вызывает ИИ сам и не использует X content для model training.
- **Обучение.** X-материалы могут участвовать в карточках, вопросах и
  повторениях как обычные `ReadingDocument`.
- **Плагины.** X source provider может быть plugin extension point, но plugin не
  должен обходить official API, compliance policy и общую anchor-модель.
- **Синхронизация.** Синхронизируются `Material`, `DocumentRevision`,
  compliance state, resources metadata, anchors, progress и annotations.
  OAuth tokens и raw API credentials синхронизируются только через отдельный
  secure secrets mechanism, если он вообще будет нужен.
- **Социальные функции.** Sharing X quotes/snapshots требует отдельной policy,
  потому что X ограничивает redistribution и требует актуальности content.

## Альтернативы

- `accepted`: официальный X API как production path.
- `accepted`: browser extension snapshot as user-initiated fallback with
  degraded compliance metadata.
- `accepted`: поддерживать и threads, и long-form content. Thread и longread -
  разные content modes одного X importer.
- `rejected`: scraping X pages или crawler-style browser automation. Это
  нарушает developer policy, нестабильно технически и ломает compliance.
- `rejected`: импортировать X через generic web-reader. X content имеет
  platform-specific source map, edits, deletions, protected/withheld status и
  API policy.
- `rejected`: official embeds как основной reader. Embeds не дают offline-first,
  стабильных anchors, unified typography и надежной индексации.
- `rejected`: считать всю conversation thread материалом для чтения. Для Lumi
  thread - авторская последовательность. Replies других пользователей - context
  или conversation layer, но не основной документ.
- `revisit`: OAuth/bookmarks в `v01`. Полезно для сохранения пользовательских
  bookmarks, но увеличивает scope: tokens, consent, privacy, user data и cost.
- `revisit`: Full-Archive Search как обязательная зависимость для thread import.
  Дает лучший импорт старых тредов, но может быть дороже и не всегда доступен.
- `revisit`: X Activity/webhooks для compliance. Может понадобиться hosted
  версии, но для early local reader достаточно периодического rehydration.

## Открытые вопросы

- Делаем ли OAuth/bookmarks частью `v01`, или сначала только public URL import?
- Какой максимальный размер thread импортировать автоматически: 25, 50, 100+
  posts?
- Как показывать partial thread: inline warning, import report или separate
  diagnostics panel?
- Нужна ли ручная сборка thread из нескольких URLs как fallback для старых
  тредов без Full-Archive Search?
- Как именно API возвращает X Article body на текущем плане, и нужен ли
  отдельный adapter для article content shape?
- Какой freshness interval нужен для compliance recheck: при каждом открытии,
  раз в сутки или только перед sync/sharing?
- Что делать с пользовательскими хайлайтами, если source X content был удален:
  оставить private note без quote, tombstone или удалить quote полностью?
- Как ограничить ИИ-функции, чтобы не нарушать запрет на training и
  чувствительные выводы по X users?

## Источники

- [Post Lookup](https://docs.x.com/x-api/posts/lookup/introduction)
- [Get Post by ID](https://docs.x.com/x-api/posts/get-post-by-id)
- [Search Posts](https://docs.x.com/x-api/posts/search/introduction)
- [Conversation ID](https://docs.x.com/x-api/fundamentals/conversation-id)
- [Data Dictionary](https://docs.x.com/x-api/fundamentals/data-dictionary)
- [Rate Limits](https://docs.x.com/x-api/fundamentals/rate-limits)
- [Usage and Billing](https://docs.x.com/x-api/fundamentals/post-cap)
- [X API pricing](https://docs.x.com/x-api/getting-started/pricing)
- [OAuth 2.0 PKCE](https://docs.x.com/fundamentals/authentication/oauth-2-0/user-access-token)
- [Developer Guidelines](https://docs.x.com/developer-guidelines)
- [Developer Policy](https://docs.x.com/developer-terms/policy)
