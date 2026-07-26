# Социальные функции

Status: accepted

## Контекст

Социальная часть Lumi нужна не как публичная лента, а как способ читать и
обсуждать материалы вместе. Для нее вводится продуктовая модель `Space`: это
не раздел навигации и не технический sync namespace, а страница и среда
конкретного субъекта в Lumi.

- `UserSpace` связан с одним аккаунтом и представляет текущий персональный опыт
  Lumi целиком.
- `CommunitySpace` представляет сообщество, клуб, группу или канал и объединяет
  нескольких пользователей вокруг материалов и совместной активности.

Reader и Desk являются отдельными рабочими поверхностями, а не видами Space.
Desk — material-centered поверхность записей и обучения внутри персонального
опыта. У Community Space есть собственная основная поверхность с материалами и
коллаборативной активностью; ее пользовательское название пока не фиксируется.
Общие material, Reader, anchor, search и UI-контракты можно переиспользовать
между персональным и community-контекстом, но Community Space не моделируется
как User Space с флагом: ownership, permissions и набор функций со временем
могут расходиться.

Первый Community Space является полупубличным: он не обнаруживается через
глобальный каталог, но доступен по ссылке. Внутри есть материалы, comments,
shared highlights, activity и общий chat. В User Space находится компактный
блок `Community` со ссылками на пространства, в которых пользователь состоит.

Ключевое ограничение: совместное чтение не должно превращаться в
распространение чужих файлов. Если в Community Space обсуждается книга, участник
видит социальные слои только для тех документов, которые он сам загрузил или
которые Lumi считает близко совпадающими с его собственной копией. Совпадение
не обязано быть byte-identical: разные электронные экземпляры одной книги
могут отличаться metadata, layout или minor formatting.

## Пользовательские сценарии

- Пользователь открывает блок `Community` в своем User Space и переходит в одно
  из пространств, в которых участвует.
- Пользователь создает Community Space для книжного клуба и делится ссылкой на
  него.
- Другой пользователь переходит по ссылке в не опубликованный в каталоге
  Community Space.
- Владелец назначает roles и управляет участниками.
- Пользователь нажимает `Share` на материале в своем User Space и выбирает
  Community Space, куда может добавлять материалы.
- Если такой материал уже есть в Community Space, Lumi связывает с ним личную
  копию пользователя вместо создания дубликата.
- Участники видят общий список обсуждаемых материалов, но открыть конкретную
  книгу могут только после загрузки своей копии или совпадающего материала.
- Два участника купили одну и ту же книгу в разных электронных магазинах. Lumi
  сопоставляет их copies by similarity и показывает общий comments layer.
- Один участник загрузил книгу, другой нет. Второй видит metadata/discussion
  shell, но не получает content file и не может прочитать private copy.
- Участники оставляют comments к anchor, главе, странице или whole material.
- Участники видят shared highlights, если автор сделал их visible.
- В Community Space есть общий chat; activity отражает системные события и не
  заменяет chat или material comments.
- Пользователь может держать личные notes private даже inside Community Space.

## Функциональные требования

### Community Spaces

Community Space содержит:

- identity: name, slug/link, description, avatar/cover;
- link access policy и lifecycle ссылки;
- members и roles;
- shared material identities;
- comments и threads;
- shared highlights;
- chat messages;
- activity events;
- optional reading milestones/challenges;
- settings и moderation rules.

Roles:

- owner;
- admin;
- member;
- read-only/guest as future option.

User-facing attribution:

- social layer показывает nickname/display name из `AccountProfile`, если он
  задан;
- права доступа, membership, ownership и audit используют stable `user_id`, а
  не nickname;
- смена nickname не меняет authorship, permissions или ссылки на пользователя.

Permissions:

- open/join Space by link according to access policy;
- invite/remove members;
- create shared material identity;
- comment;
- moderate/delete comments;
- share highlight;
- view activity.

### Доступ по ссылке

Первый Community Space использует полупубличную модель:

```text
discoverability: unlisted
entry: by_link
```

Space не обязан отображаться в глобальном каталоге или поиске сообществ, но
человек со ссылкой может открыть его и пройти предусмотренный flow входа.
Ссылка должна быть отзывной и перевыпускаемой. Открытие ссылки показывает
безопасный preview с name, description и числом участников. Membership
создаётся только после явного действия `Вступить`. Preview не возвращает
identities участников, материалы, activity или social content. Право
комментировать, писать в chat и добавлять материалы требует active membership
и проверяется сервером.

Секрет ссылки передаётся в fragment URL. Browser извлекает его и отправляет
только в bounded JSON body preview/join; token не попадает в HTTP request URL,
Referer или application logs. В PostgreSQL хранится cryptographic hash, а
зашифрованный envelope нужен только для deterministic idempotent replay
create/rotate response. После успешного join Web заменяет fragment route и тем
самым очищает token из адресной строки.

`left` membership может снова стать active по действующей ссылке. `removed`
membership не восстанавливается общей ссылкой: нужен отдельный будущий
moderation/invite command. Owner не может выйти, быть удалён или понижен без
явной передачи ownership.

Доступ по ссылке не дает доступа к приватным source blobs, личным notes,
прогрессу или learning state участников.

### Share материала из User Space

Карточка материала, его detail surface и Reader должны предоставлять действие
`Share`. Оно открывает выбор Community Spaces, в которых пользователь состоит и
имеет право добавлять материалы.

Операция:

1. Пользователь выбирает один Community Space.
2. Lumi ищет существующий `SharedMaterialIdentity` по metadata и fingerprints.
3. Если identity уже существует, личная копия связывается с ней через
   `UserMaterialClaim`.
4. Если identity отсутствует, Lumi создает ее из разрешенных metadata,
   fingerprints и source descriptors.
5. Source blob, private annotations и другие данные User Space в Community
   Space не копируются.

Один материал можно последовательно добавить в несколько Community Spaces.
Удаление материала из Space или выход пользователя не удаляет его личный
`Material`. `Share material to Space` является базовой операцией первого
социального среза и не означает рекомендацию или публикацию файла.

### Material access и matching

Community Space по умолчанию не распространяет source material blobs.

Для каждого shared material:

1. Space stores `SharedMaterialIdentity`: title, creators, normalized metadata,
   fingerprints и optional source descriptors.
2. Each user can create `UserMaterialClaim`, связывая свой local `Material`.
3. Lumi computes similarity/match between local material и shared identity.
4. If match passes threshold, user can view social layer anchored to their
   copy.
5. If not matched, UI asks user to import their own copy.

Matching signals:

- ISBN/DOI/canonical URL where available;
- normalized title/authors/publisher/year;
- content fingerprints из extracted text;
- chapter/heading sequence;
- MinHash/SimHash shingles;
- PDF page text fingerprints;
- normalized-text fingerprints для каждого фактически поддержанного importer
  family, включая EPUB, Web, Telegram, Markdown, `.lum` и FB2 после выпуска
  соответствующего importer;
- source URL for web materials.

Byte identity is sufficient but not required. Similarity threshold must be
conservative to avoid cross-book leakage.

### Anchor mapping across copies

Comments are created against a user's local anchor. For shared display:

- store source quote, prefix/suffix, section/page metadata и material
  fingerprint location;
- map to other user's document revision using anchor recovery;
- if mapping confidence is high, show inline;
- if confidence is low, show in side panel with "unresolved location";
- never expose original file bytes to solve mapping.

PDF-specific:

- exact page numbers may differ between editions;
- use text quote/context и page label where possible;
- coordinate overlays only apply to the creator's exact revision unless
  mapping confirms corresponding text region.

### Comments, highlights и chat

Social entities:

- material-level comments;
- anchor-level comments;
- threaded replies;
- shared highlights;
- Space-level chat messages;
- activity events: joined, added material identity, completed chapter, started
  discussion, etc.

Personal notes are not social comments. User can convert/share selected note или
highlight explicitly.

До готовности Records v2 отдельно выпускается material-level discussion
contract из [`ADR 0032`](../adr/0032-material-discussions-and-moderation.md):

- active member видит и создаёт discussion целого материала даже без matched
  claim, потому что ответ не содержит quote или текст книги;
- thread создаётся вместе с первым comment, replies имеют один уровень;
- автор редактирует/удаляет своё с `expected_revision`;
- owner/admin скрывает, восстанавливает или удаляет social content;
- delete оставляет tombstone, hide маскирует body для обычного участника;
- выдача cursor-paginated и не содержит anchor, source annotation, private
  material/revision или normalized package.

Эта capability называется `material-discussions`. Она не публикует
`shared-reading`: anchor comments, shared highlights и Reader overlay остаются
зависимыми от общего target/provenance contract `0.4.0`.

Chat и comments имеют разные контексты:

- comment является устойчивым обсуждением материала, главы или anchor;
- chat является общей коммуникацией Community Space без обязательной привязки
  к материалу;
- activity содержит системные события и не является третьей пользовательской
  лентой сообщений.

Capability `community-communications` фиксирует первый release contract:

- chat CRUD использует active membership, author-only edit/delete,
  `expected_revision`, idempotency и общие moderation tombstones;
- activity является append-only projection и возвращает только allowlisted
  kinds/subjects без body, source metadata и fingerprint payload;
- chat/activity имеют cursor pages до 100 объектов и Web polling с паузой в
  hidden tab, backoff после ошибки и ручным retry;
- account-scoped MCP adapters используют те же `SocialRuntime` services и
  маскируют forbidden cross-Space resource как not found;
- решение и границы описаны в
  [`ADR 0033`](../adr/0033-community-chat-activity-and-mcp.md).

### Privacy controls

- Default notes/highlights are private.
- Sharing a highlight requires explicit action or per-Space setting.
- Reading progress visibility is opt-in.
- Learning attempts are private by default.
- AI-generated summaries can be shared only after user accepts/shares them.
- Community Space search returns only shared content and user's own matched
  material snippets, not other users' private files.

### Moderation and deletion

- Author can edit/delete own comments.
- Admin/owner can moderate comments.
- Deletes create tombstones for sync consistency.
- Export/audit should show who created shared content and when.
- Выход из Space не удаляет уже опубликованный social content: оно сохраняет
  stable authorship до удаления автором, moderator action или account deletion
  policy.
- Удалённый account отображается как `Удалённый пользователь`; ACL и
  provenance продолжают ссылаться на stable `user_id`, а не nickname.

## Нефункциональные требования

- **Copyright safety.** Social layer must not give file/content access to users
  who did not supply their own matching material.
- **Privacy by default.** Personal notes, progress and attempts remain private.
- **Resilient anchors.** Shared comments should survive different copies and
  revisions where possible.
- **Access control.** Server enforces membership and material claim checks.
- **Offline tolerance.** Users can draft comments offline; posting happens when
  synced.
- **Transparency.** UI should clearly distinguish private notes from shared
  comments.
- **Moderation.** Community Spaces need enough controls to remove bad content.

## Модель данных

```text
UserSpace
  -> CommunityMembershipRef[]

CommunitySpace
  -> CommunitySpaceMember[]
  -> CommunitySpaceAccessLink[]
  -> SharedMaterialIdentity[]
  -> UserMaterialClaim[]
  -> SharedCommentThread[]
  -> SharedChatMessage[]
  -> SharedActivityEvent[]
```

Основные сущности:

- `UserSpace` - продуктовая страница и персональная среда одного account; не
  заменяет account aggregate и не переносит все личные объекты под новый
  ownership root.
- `CommunityMembershipRef` - projection для блока `Community` в User Space.
- `CommunitySpace` - страница и collaborative environment сообщества.
- `CommunitySpaceMember` - user, role, status.
- `CommunitySpaceAccessLink` - отзываемый link access token/policy.
- `AccountProfileRef` - display metadata пользователя для подписи comments and
  activity, связанная со stable `user_id`.
- `SharedMaterialIdentity` - abstract material in Community Space.
- `MaterialFingerprint` - normalized metadata and content fingerprints.
- `UserMaterialClaim` - user's local material matched to shared identity.
- `SharedAnchor` - portable social anchor with quote/context/mapping data.
- `SharedCommentThread` - comments around material/anchor.
- `SharedComment` - threaded message.
- `SharedHighlight` - user-visible highlight.
- `SharedChatMessage` - Community Space-level chat.
- `SharedActivityEvent` - event stream.
- `ModerationAction` - delete/hide/warn/etc.

Community Space:

```text
CommunitySpace {
  id
  slug
  name
  description?
  avatar_ref?
  cover_ref?
  discoverability: unlisted
  entry: by_link
  created_by_user_id
  created_at
}

CommunitySpaceAccessLink {
  id
  community_space_id
  token_hash
  status: active | revoked
  created_by_user_id
  created_at
  expires_at?
}
```

Material claim:

```text
UserMaterialClaim {
  id
  shared_material_id
  user_id
  material_id
  document_revision_id
  match_status: pending | matched | rejected | manual_review
  match_score
  fingerprint_version
  created_at
}
```

Shared anchor:

```text
SharedAnchor {
  id
  shared_material_id
  creator_material_id
  creator_document_revision_id
  creator_anchor
  quote
  prefix_context
  suffix_context
  heading_path
  page_label
  content_fingerprint
}
```

## Реализация

### Fingerprinting pipeline

1. Importer creates normalized text layer.
2. Server computes a versioned metadata/content fingerprint. В E2 explicit
   share/claim/recheck делает это для текущей immutable active revision;
   durable Job/backfill подключается после стабилизации revision lifecycle
   `0.4.0`.
3. Community Space claim compares local fingerprint to shared identity.
4. Server stores match score/status и server-internal evidence, но не raw full
   text.
5. Client maps shared anchors to local document revision.

Принятый `material-fingerprint.v1` описан в
[`ADR 0031`](../adr/0031-material-fingerprints-and-community-claims.md):
canonical exact hash дополняется 32-lane MinHash по нормализованным shingles,
защищённым версионированным server-side HMAC key. Raw/protected signatures,
metadata key и section hash никогда не возвращаются клиенту. Exact non-empty
content совпадает автоматически; similarity требует не менее 100 tokens,
90% MinHash similarity, совместимый размер и metadata/section evidence.
Metadata-only и неоднозначные случаи дают `manual_review`, а не `matched`.

### Community Space sync

Community Space is a `SyncSpace`:

- membership and permissions stored server-side;
- shared comments/chat/activity sync to members;
- personal copies of materials remain in personal `SyncSpace`;
- Community Space references personal `UserMaterialClaim`, but does not own the
  material blob.

Продуктовый `UserSpace`/`CommunitySpace` и инфраструктурный `SyncSpace` не
являются одной сущностью. `SyncSpace` задает namespace доставки и access
control; продуктовый Space задает страницу, identity и социальное поведение.

### Posting comment

1. User selects anchor/material.
2. Client checks local material claim.
3. Creates `SharedAnchor` or reuses existing mapped anchor.
4. Adds comment to local shared outbox.
5. Server validates membership and matched claim.
6. Other clients receive comment and map anchor to their local copy.

### Share material to Space

Options:

- основная операция: user вызывает `Share` для local `Material` и выбирает
  Community Space;
- user creates metadata-only shared material manually;
- повторный `Share` на существующую identity создает или обновляет
  `UserMaterialClaim`, а не дубликат материала.

Команда должна быть permission-checked и idempotent. When created from local
material, server may store metadata/fingerprints, not the source blob for other
users.

Application command:

```text
share_material_to_space(
  material_id,
  community_space_id,
  idempotency_key
) -> SharedMaterialIdentity + UserMaterialClaim
```

### Будущие community-разделы

В будущем Community Space может получить самостоятельные разделы:

- blog/publications;
- forum/topics;
- marketplace;
- material recommendations and discovery;
- shared knowledge or другие community-механики.

Это список возможных направлений, а не target contract текущего среза. Их
точный состав, data models, permissions и UX не фиксируются и могут быть
пересмотрены. В первый срез из коммуникационных поверхностей входят только
material comments и общий chat.

## Интеграции и зависимости

- **Reader.** Displays shared comments/highlights as separate overlay layer.
- **User Space.** Показывает блок `Community` и предоставляет `Share` на
  material surfaces.
- **Синхронизация.** Community Spaces are shared sync spaces with access control.
- **Blobs.** Avatar/cover используют общий blob/attachment contract с
  Space-scoped authorization, validation, retention и GC; social не вводит
  отдельный uploader.
- **Веб-аккаунт.** `user_id` and `AccountProfile.nickname` приходят из
  [`web-account.md`](web-account.md); nickname используется только как
  display-подпись.
- **Поиск.** Search respects Space membership and material claim status.
- **База знаний.** Users may turn shared comments into private KB notes; this
  should not expose other users' private content.
- **Learning.** Shared challenges/milestones can be added later; attempts stay
  private by default.
- **ИИ.** AI can summarize shared discussion only over content user can access
  plus shared comments.
- **MCP.** Space, sharing, comment and chat tools use the same
  membership/claim/moderation checks and application services as Web.
- **Плагины.** Plugins can add Community Space widgets/actions only with social
  capabilities and access checks.

## Альтернативы

- `rejected`: Community Space distributes uploaded book file to all members. This
  creates copyright and trust problems.
- `rejected`: require byte-identical files for collaboration. Too brittle for
  normal ebook/PDF variations.
- `rejected`: public social feed as primary social surface. It distracts from
  reading and increases moderation scope.
- `accepted`: first slice Community Space is unlisted and accessible by link,
  without a global public directory.
- `deferred`: blog, forum, marketplace, recommendations and other community
  sections. They remain future ideas without current contracts.
- `revisit`: server-side full-text matching over uploaded content. Better
  matching, but privacy/legal tradeoffs need review.
- `revisit`: real-time collaborative annotations. Useful later; async comments
  and sync are enough for initial social design.

## Открытые вопросы

- How should quoted snippets in comments be limited to avoid reconstructing a
  book through many comments?
