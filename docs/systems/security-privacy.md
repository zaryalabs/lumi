# Security и privacy

Status: accepted

## Контекст

Lumi работает с личной библиотекой, заметками, голосовыми ответами, AI context,
социальными комментариями и импортом из внешних источников. Поэтому безопасность
и privacy должны быть архитектурными требованиями, а не UI-настройками в конце.

## Threat model

Основные риски:

- malicious EPUB/FB2/PDF/HTML/Markdown input;
- SSRF через URL import and cloud browser capture;
- supply-chain compromise in parser/render/search/plugin dependencies;
- утечка AI API keys and OAuth/provider tokens;
- server compromise for cloud-backed web accounts;
- overprivileged MCP agent or plugin;
- accidental public sharing of private notes/materials;
- X/Telegram policy violations;
- lost seed phrase/devices;
- corrupted sync aggregate, migration or normalized package.

## Data classification

| Класс | Примеры | Server visibility |
| --- | --- | --- |
| Cloud web private content | web library, notes, highlights, blobs, search index | plaintext or service-readable according to cloud account policy |
| Native private vault | local-only materials, notes, learning history | local by default; future private mode uses encrypted relay only |
| Account metadata | user id, auth verifier/public material, sessions, devices | plaintext minimized server state |
| Provider secrets | API keys, OAuth tokens, Telegram links | secret storage only; not ordinary sync plaintext |
| Social/shared content | shared comments, visible highlights, room activity | visible according to room membership and room policy |
| Public/share content | published cards, explicitly shared notes/quotes | plaintext by user intent |
| Operational metadata | request/job timings, errors, quotas | redacted; no content bodies in logs |

## Privacy modes

### Cloud-backed web mode

First web target stores account content on the server. Hosted AI, server-side
search and server import jobs may access content only under explicit product
policy and user-facing controls.

### Native full-copy mode

Desktop/mobile store local copies, local blobs and local indexes. Server assists
sync, backup/export and social coordination.

### Future private/decentralized mode

Native clients can disable cloud replica for private vault:

- private content stays on user devices and user-controlled backups;
- server stores account/device/relay/social metadata and encrypted envelopes;
- raw seed phrase never leaves user possession;
- hosted AI/server search are disabled unless user sends selected context;
- loss of all devices without export/backup/recovery can mean unrecoverable
  data loss.

## Security measures

- No imported scripts, inline handlers, unsafe iframes or arbitrary JS in reader
  content.
- Sanitizer allowlists and URL rewriting for HTML/SVG-like inputs.
- ZIP/XML/HTML/PDF size limits, cancellation, parser fuzzing and quarantine.
- SSRF protection: private/link-local/loopback/cloud metadata endpoints blocked,
  with DNS and redirect rechecks.
- API keys and OAuth tokens stored via secure local/server secret storage, not
  sync plaintext.
- Plugins and MCP agents use explicit capabilities, scoped tools, audit and
  approval for writes.
- Public sharing uses preview, quote/source limits and revocation where
  possible.
- Destructive migrations require backup/snapshot strategy.
- Non-local web deployment fail-closed требует HTTPS origin, matching auth
  audience и Secure cookies.
- Telegram bot token проверяется через provider, хранится как AEAD-шифротекст и
  не возвращается через API; отдельный local master key не хранится в
  PostgreSQL. Встроенный long polling использует durable idempotent handler.
- Readiness проверяет migration compatibility и bounded blob
  write/rename/read/delete sentinel; backup связывает quiesced PostgreSQL и blob
  artifacts manifest/checksums и проверяется disposable restore drill.

## Privacy UX

Before these actions, UI must explain what data leaves the device/account and
who can access it:

- AI task/chat/explain-back;
- Telegram linking/import;
- web capture/browser extension upload;
- public share;
- shared room publish;
- switching from cloud mode to private/decentralized mode.

## Открытые вопросы

- Exact E2EE model for private mode and encrypted relay.
- Recovery UX if all devices are lost.
- Hosted AI retention and provider routing policy.
- Quote limits and copyright policy for shared/public snippets.
