# ADR 0037: fingerprints материалов и conservative Community claims

Status: accepted

## Контекст

Community Space должен позволять двум участникам обсуждать один материал поверх
собственных копий, не передавая source blob, normalized package или полный
текст чужому аккаунту. Byte equality недостаточно: EPUB, Markdown, LUM и PDF
одного текста могут различаться упаковкой, metadata и layout. При этом false
positive опаснее false negative, потому что matched claim позднее открывает
anchor-level social layer.

`0.4.0` ещё меняет records/open-target и revision lifecycle. Поэтому E2 нужен
рабочий contract для текущих immutable normalized revisions без второго
record/index контура и без предположения, что фоновый backfill уже стабилен.

## Решение

`MaterialFingerprint` является server-internal rebuildable projection одной
immutable `DocumentRevision`. Версия `material-fingerprint.v1` строится из:

- normalized title и упорядоченного множества creators;
- canonical word stream без layout и пунктуации;
- ordered section/heading labels;
- exact canonical text SHA-256;
- 32-lane MinHash по пятисловным shingles;
- token count и логарифмического length bucket.

Shingles проходят HMAC-SHA256 с feature key, производным из внешнего
versioned `SecretStore` key ring. В таблице сохраняется `key_version`.
Raw shingles, canonical text и feature key не сохраняются. Protected
signature, metadata/content hashes и внутренний `match_evidence` не входят в
HTTP DTO, activity, sync payload logs или browser state.

Key rotation не смешивает несовместимые signatures: полная версия evidence
содержит algorithm и key version. Новая версия требует rebuild; до него
сравнение не выполняется.

### Порядок решения

1. Exact canonical content при ненулевом тексте даёт `matched`.
2. High similarity даёт `matched` только для обеих копий длиной не менее 100
   tokens, совместимого length bucket и score не ниже 90%, дополненного
   совпадением metadata или section sequence.
3. Metadata equality или similarity от 60% даёт `manual_review`, но не
   matched access.
4. Остальные случаи получают `rejected`.

Один title, creator или близкий размер никогда не дают automatic match. Для
короткого текста automatic similarity отключён; exact content остаётся
допустимым.

### Share и claim

Authenticated client передаёт только собственный `material_id`.
`SocialStore` сам проверяет active membership, owner scope материала, ready
active revision и normalized package. Клиент не может передать fingerprint,
score, status или чужой revision.

`share_material_to_space` сериализуется Space row lock:

1. повтор существующего claim возвращает тот же identity;
2. exact/high candidate переиспользуется;
3. ambiguous candidate получает `manual_review`;
4. при отсутствии candidate создаётся `SharedMaterialIdentity` и creator claim;
5. materialized rows, community sync change, payload-free `material_added`,
   idempotent response и claim сохраняются в одной транзакции.

`SharedMaterialIdentity` содержит только title, creators и source-family
descriptors. `UserMaterialClaim.material_id` возвращается только текущему
владельцу claim. List/detail всегда проходят membership scope. Source/package
routes остаются personal-owner scoped независимо от social claim.

## Importer coverage

Текущий extractor читает общий `NormalizedContentPackage` для EPUB, Web,
Telegram, Markdown и LUM и `FixedLayoutContentPackage.text_layers` для PDF.
Capability не обещает FB2, пока importer отсутствует.

Fingerprint вычисляется при `share`, подключении собственной копии и `recheck`.
Это даёт законченный пользовательский E2 outcome для текущей active revision,
но не подменяет будущий общий background lifecycle.

## Отложенная интеграция с 0.4.0

До release evidence 0.4.0 остаются выключенными:

- enqueue `Job(kind = material_fingerprint)` из каждого successful import и
  revision switch;
- массовый backfill всех ready revisions и автоматическая re-evaluation claims;
- enriched ISBN/DOI/canonical URL evidence из будущего metadata/record contract;
- invalidation/переоценка, связанная с open-target и search index lifecycle.

Точный список хранится в
[`0.5.0-deferred-until-0.4.0.md`](../tmp-plans/0.5.0-deferred-until-0.4.0.md).
Эти пункты не нужны для явного share/recheck текущей immutable copy и не
публикуются отдельными capabilities.

## Последствия

- Source blob и полный normalized text не переходят в community ownership.
- False negative приводит к `manual_review`; false positive не маскируется.
- Sync/API остаются компактными и не содержат reconstructable shingle set.
- Share временно выполняет bounded CPU work в request path. Переход на общий
  job runtime обязателен после стабилизации 0.4.0 revision hooks.
- `material-sharing` публикуется только persistent server, где доступны
  PostgreSQL projection, normalized packages и versioned feature key.

## Compatibility

Forward-only migration
`20260726310000_material_sharing_matching.sql` добавляет fingerprints,
shared identities и claims, не меняя personal materials/revisions.

Public contract остаётся под `/api/v1/spaces/{space_id}/materials*`.
Fingerprint payload не является public API и может быть rebuild-нут новой
algorithm/key version.
