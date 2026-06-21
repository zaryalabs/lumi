# Социальные функции

Status: draft

## Контекст

Социальная часть Lumi нужна не как публичная лента, а как способ читать и
обсуждать материалы вместе. Основная модель - общие папки: пользователь
создает пространство, добавляет участников, а внутри появляются comments,
shared highlights, activity и chat вокруг материалов.

Ключевое ограничение: совместное чтение не должно превращаться в
распространение чужих файлов. Если в общей папке обсуждается книга, участник
видит социальные слои только для тех документов, которые он сам загрузил или
которые Lumi считает близко совпадающими с его собственной копией. Совпадение
не обязано быть byte-identical: разные электронные экземпляры одной книги
могут отличаться metadata, layout или minor formatting.

## Пользовательские сценарии

- Пользователь создает shared folder для книжного клуба.
- Пользователь приглашает участников и назначает roles.
- Участники видят общий список обсуждаемых материалов, но открыть конкретную
  книгу могут только после загрузки своей копии или совпадающего материала.
- Два участника купили одну и ту же книгу в разных электронных магазинах. Lumi
  сопоставляет их copies by similarity и показывает общий comments layer.
- Один участник загрузил книгу, другой нет. Второй видит metadata/discussion
  shell, но не получает content file и не может прочитать private copy.
- Участники оставляют comments к anchor, главе, странице или whole material.
- Участники видят shared highlights, если автор сделал их visible.
- В shared folder есть общий chat/activity stream.
- Пользователь может держать личные notes private даже inside shared folder.

## Функциональные требования

### Shared folders

Shared folder содержит:

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

- invite/remove members;
- create shared material identity;
- comment;
- moderate/delete comments;
- share highlight;
- view activity.

### Material access и matching

Shared folder по умолчанию не распространяет source material blobs.

Для каждого shared material:

1. Folder stores `SharedMaterialIdentity`: title, creators, normalized metadata,
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
- EPUB/FB2 normalized text fingerprints;
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
- chat messages inside folder;
- activity events: joined, added material identity, completed chapter, started
  discussion, etc.

Personal notes are not social comments. User can convert/share selected note или
highlight explicitly.

### Privacy controls

- Default notes/highlights are private.
- Sharing a highlight requires explicit action or per-folder setting.
- Reading progress visibility is opt-in.
- Learning attempts are private by default.
- AI-generated summaries can be shared only after user accepts/shares them.
- Shared folder search returns only shared content and user's own matched
  material snippets, not other users' private files.

### Moderation and deletion

- Author can edit/delete own comments.
- Admin/owner can moderate comments.
- Deletes create tombstones for sync consistency.
- Export/audit should show who created shared content and when.

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
- **Moderation.** Shared spaces need enough controls to remove bad content.

## Модель данных

```text
SharedFolder
  -> SharedFolderMember[]
  -> SharedMaterialIdentity[]
  -> UserMaterialClaim[]
  -> SharedCommentThread[]
  -> SharedChatMessage[]
  -> SharedActivityEvent[]
```

Основные сущности:

- `SharedFolder` - collaborative space.
- `SharedFolderMember` - user, role, status.
- `AccountProfileRef` - display metadata пользователя для подписи comments and
  activity, связанная со stable `user_id`.
- `SharedMaterialIdentity` - abstract material in shared folder.
- `MaterialFingerprint` - normalized metadata and content fingerprints.
- `UserMaterialClaim` - user's local material matched to shared identity.
- `SharedAnchor` - portable social anchor with quote/context/mapping data.
- `SharedCommentThread` - comments around material/anchor.
- `SharedComment` - threaded message.
- `SharedHighlight` - user-visible highlight.
- `SharedChatMessage` - folder-level chat.
- `SharedActivityEvent` - event stream.
- `ModerationAction` - delete/hide/warn/etc.

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
2. Fingerprint job computes metadata fingerprint and text shingles.
3. Shared folder claim compares local fingerprint to shared identity.
4. Server stores match score/status, not necessarily raw full text.
5. Client maps shared anchors to local document revision.

Open privacy choice: exact fingerprint payload must be designed so it is useful
for matching but does not become a practical substitute for the text.

### Shared space sync

Shared folder is a `SyncSpace`:

- membership and permissions stored server-side;
- shared comments/chat/activity sync to members;
- personal copies of materials remain in personal space;
- shared folder references personal `UserMaterialClaim`, but does not own the
  material blob.

### Posting comment

1. User selects anchor/material.
2. Client checks local material claim.
3. Creates `SharedAnchor` or reuses existing mapped anchor.
4. Adds comment to local shared outbox.
5. Server validates membership and matched claim.
6. Other clients receive comment and map anchor to their local copy.

### Shared material creation

Options:

- user creates shared material from local `Material`;
- user creates metadata-only shared material manually;
- invite link includes shared material identity but no content file.

When created from local material, server may store metadata/fingerprints, not
the source blob for other users.

## Интеграции и зависимости

- **Reader.** Displays shared comments/highlights as separate overlay layer.
- **Синхронизация.** Shared folders are shared sync spaces with access control.
- **Веб-аккаунт.** `user_id` and `AccountProfile.nickname` приходят из
  [`web-account.md`](web-account.md); nickname используется только как
  display-подпись.
- **Поиск.** Search respects folder membership and material claim status.
- **База знаний.** Users may turn shared comments into private KB notes; this
  should not expose other users' private content.
- **Learning.** Shared challenges/milestones can be added later; attempts stay
  private by default.
- **ИИ.** AI can summarize shared discussion only over content user can access
  plus shared comments.
- **Плагины.** Plugins can add shared folder widgets/actions only with social
  capabilities and access checks.

## Альтернативы

- `rejected`: shared folder distributes uploaded book file to all members. This
  creates copyright and trust problems.
- `rejected`: require byte-identical files for collaboration. Too brittle for
  normal ebook/PDF variations.
- `rejected`: public social feed as primary social surface. It distracts from
  reading and increases moderation scope.
- `revisit`: server-side full-text matching over uploaded content. Better
  matching, but privacy/legal tradeoffs need review.
- `revisit`: real-time collaborative annotations. Useful later; async comments
  and sync are enough for initial social design.

## Открытые вопросы

- What exact fingerprint format balances matching quality and text privacy?
- Which similarity threshold is safe enough across EPUB/FB2/PDF editions?
- Should metadata-only shared material pages show discussion to users without a
  matching local copy, or only invite them to import?
- How should quoted snippets in comments be limited to avoid reconstructing a
  book through many comments?
- Do shared folders need public links or only explicit invites?
