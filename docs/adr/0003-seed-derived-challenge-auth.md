# ADR 0003: Seed-derived challenge signing для web-auth

Status: accepted

## Контекст

S0 использует заменяемую границу `ReplaceableChallengeSigningSha256`, чтобы
account-owned API не зависел от финального auth-протокола. Для S1 нужен
production boundary, который:

- не отправляет и не хранит seed phrase на сервере;
- позволяет восстановить доступ на новом устройстве только по seed phrase;
- не оставляет на сервере password-equivalent verifier для offline-перебора;
- одинаково реализуем в web, desktop и mobile клиентах;
- поддерживает отзывные web-сессии и не меняет стабильный `user_id`.

Seed phrase создаётся Lumi из криптографически случайной 256-битной энтропии,
поэтому это не пользовательский пароль с низкой энтропией. Это делает
public-key challenge signing проще и безопаснее для принятой модели, чем PAKE,
основное преимущество которого проявляется для запоминаемых паролей.

## Решение

Использовать для S1 seed-derived Ed25519 challenge signing:

1. Клиент генерирует 256 бит энтропии и кодирует их 24 английскими словами
   BIP39. Пользовательские и укороченные фразы не принимаются.
2. После проверки checksum клиент декодирует исходную entropy и через
   HKDF-SHA-256 с salt `lumi-auth-v1` выводит два независимых значения:
   `signing-key` и `account-lookup-key`.
3. Ed25519 signing key строится из `signing-key`. На сервер передаются только
   public key и `lookup_id = SHA-256(account-lookup-key)`. Сервер создаёт
   стабильный `user_id` как UUIDv7; `lookup_id` является отдельным уникальным,
   непоказываемым идентификатором auth identity и никогда не заменяет `user_id`.
4. Login challenge является versioned binary transcript с domain separator
   `LUMI-AUTH-V1`, `challenge_id`, `lookup_id`, audience, случайным 256-битным
   nonce и сроком действия. До подписи клиент обязан разобрать transcript и
   проверить version/domain, совпадение `lookup_id`, exact trusted origin в
   audience и TTL не более пяти минут. После проверки клиент подписывает точные
   исходные байты; JSON не используется как подписываемая каноническая форма.
5. Challenge живёт не более пяти минут, имеет лимит попыток и потребляется
   атомарно при успешной проверке. Повторное доказательство отклоняется.
6. Неизвестный `lookup_id` получает внешне неотличимый synthetic challenge и
   общий ответ об ошибке. Challenge и verify endpoints ограничиваются по IP и
   lookup bucket.
7. Успешный login создаёт случайный 256-битный opaque session token. В базе
   хранится только SHA-256 token hash. Cookie называется
   `__Host-lumi_session` и имеет `Secure`, `HttpOnly`, `SameSite=Lax`, `Path=/`
   без `Domain`.
8. Mutating requests дополнительно проверяют same-origin `Origin`/`Referer` и
   session-bound CSRF token в `X-Lumi-CSRF`. Session меняется после login и
   security-sensitive изменений, может быть отозвана отдельно или вместе со
   всеми сессиями аккаунта.

Первая версия поддерживает одну активную seed identity и несколько sessions /
devices. Таблица auth identities остаётся one-to-many, чтобы позднее добавить
key rotation или дополнительный recovery factor без смены `user_id`.

## Последствия

- Компрометация auth-базы не даёт material для входа: public key и lookup id не
  являются signing secret.
- Потеря seed phrase без активного устройства или будущего recovery factor
  означает потерю доступа; registration UX обязан потребовать подтверждение
  сохранения 24 слов.
- Клиент должен очищать entropy, derived keys и signing key из памяти настолько
  быстро, насколько позволяет платформа. Seed нельзя писать в logs, analytics,
  crash reports, browser storage или server payloads.
- Ed25519 и HKDF должны иметь одинаковые test vectors во всех клиентах.
- Возможные будущие E2EE keys выводятся из seed entropy только по отдельному ADR,
  с независимым salt/domain separation; auth-derived keys для шифрования vault
  не переиспользуются.
- Auth не защищает от скомпрометированного клиента или phishing UI; TLS,
  trusted origin, CSP и supply-chain controls остаются обязательными.

## Альтернативы

- OPAQUE/PAKE: отклонено для S1. Оно подходит для низкоэнтропийных passwords,
  но добавляет более сложный state machine и interoperability burden без
  преимущества для сгенерированного 256-битного credential.
- Передавать seed как обычный password в TLS и хранить Argon2id hash: отклонено,
  потому что сервер видит raw credential и получает password-equivalent
  verifier.
- WebAuthn как единственный вход: отклонено, потому что не обеспечивает
  принятую переносимость и восстановление только по seed phrase. Позже WebAuthn
  можно добавить как удобный дополнительный factor.
- Подписывать JSON request целиком: отклонено из-за риска различий
  канонизации. Подписываются точные versioned challenge bytes.

## Compatibility

- ADR 0002 остаётся историей S0 и помечается `superseded`.
- Stage 1 добавляет новую algorithm marker, отдельные `auth_identities`,
  `auth_challenges`, `web_sessions` и migration из fixture-only prototype data.
  Prototype verifier не повышается автоматически до production identity.
- Исполняемый spike находится в `spikes/stage0/src/auth.rs` и проверяет
  независимый key derivation, клиентскую проверку challenge context, успешную
  подпись, изменение audience и защиту от replay.
- Нужны фиксированные cross-platform vectors для entropy, lookup id, public
  key, challenge bytes и signature.

Источники: [RFC 5869](https://www.rfc-editor.org/rfc/rfc5869),
[RFC 8032](https://www.rfc-editor.org/rfc/rfc8032) и
[BIP 39](https://github.com/bitcoin/bips/blob/master/bip-0039.mediawiki).
