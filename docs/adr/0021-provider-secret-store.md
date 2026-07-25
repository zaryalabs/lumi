# ADR 0021: account-scoped provider secrets

Status: accepted

## Контекст

Web-режим `0.2.0` хранит пользовательский OpenRouter BYOK server-side.
Credential нельзя возвращать после сохранения, синхронизировать plaintext,
передавать MCP-агенту или случайно включать в logs. Уже реализован Telegram
AEAD store с локальным master key, но его singleton-specific код нельзя
копировать для каждого provider.

## Решение

- `SecretStore` становится reusable server boundary. В PostgreSQL хранится
  envelope: `secret_id`, owner, purpose/provider, ciphertext, nonce,
  `key_version`, keyed fingerprint, timestamps и lifecycle state.
- Шифрование — AES-256-GCM из уже принятого `ring`; AAD включает instance id,
  owner account id, secret id, purpose/provider и key version. Перестановка
  ciphertext между accounts/rows не расшифровывается.
- Master keys находятся только во внешнем persistent secret root/production
  secret manager, никогда в PostgreSQL. Key ring содержит один active encrypt
  key и ограниченный набор decrypt-only старых versions.
- Rotation добавляет новый active key и batch-rewrap с optimistic locking.
  Чтение старой envelope может выполнить lazy rewrap. Старый key удаляется
  только после database scan и backup/restore drill, подтверждающих отсутствие
  envelopes этой version.
- Provider credential имеет scope
  `account + provider_kind`; в `0.2.0` допускается один active credential на
  OpenRouter. Замена сначала проверяет новый ключ через bounded provider call,
  затем атомарно активирует новую envelope и уничтожает старую.
- Fingerprint вычисляется keyed hash, пригоден только для equality/diagnostics
  и показывается сокращенно. Plain SHA ключа и последние символы credential не
  хранятся как идентификатор.
- Create response может вернуть только состояние `configured`, fingerprint
  prefix и validation metadata. Secret value показывается UI только до
  отправки; read/list, sync, diagnostics, metrics и MCP никогда его не содержат.
- Delete делает credential немедленно недоступным, zeroize-ит временные buffers
  и удаляет envelope. Backups могут содержать только ciphertext и требуют
  отдельной защиты key material.
- Secret-bearing types имеют redacted `Debug`; HTTP/provider middleware
  запрещает body/header logging. `Authorization`, prompt bodies и provider raw
  errors проходят allowlist redaction.

## Последствия

- Telegram и AI используют один crypto/key-rotation contract, сохраняя разные
  authorization policies.
- Потеря всех master keys не раскрывает credentials, но требует повторной
  настройки providers после restore.
- Self-hosted operator обязан сохранять secret root отдельно от PostgreSQL
  backup и проверять совместное восстановление.

## Альтернативы

- Plaintext или reversible encoding в PostgreSQL: отклонено.
- Хранить BYOK в browser local storage: отклонено для server-side streaming и
  durable worker; client-local credentials остаются будущим native mode.
- Один unversioned key: отклонено, потому что безопасная rotation невозможна.
- Показывать credential повторно через settings API: отклонено.

## Совместимость и проверки

- Первый reusable envelope profile: `secret-envelope.aes256gcm.v1`.
- Required tests: wrong account/purpose AAD, tampered ciphertext, rotation и
  lazy rewrap, concurrent replace/delete, plaintext database/log/diagnostic
  scan, backup/restore с key ring и отсутствие secret во всех MCP/API reads.
