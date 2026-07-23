# ADR 0015: portable `.lum` package import

Status: accepted

## Контекст

Канонический `lum` описывает две формы: plain-text source project и переносимый
ZIP package. Текущий Web/API import принимает один immutable upload, а общий
Markdown compiler уже умеет детерминированно компилировать одну главу без
создания storage state.

## Решение

- Первый user-facing срез принимает `.lum` ZIP с media type
  `application/vnd.lumi.lum+zip`. Импорт source project остаётся build/CLI
  границей: Web не вводит непереносимый directory-upload contract.
- Container разрешает только stored/Deflate entries, нормализованные UTF-8
  paths и versioned limits. Symlinks, encryption, duplicate/case-colliding
  paths, `..`, абсолютные paths и подозрительное сжатие отклоняются.
- `lum.toml` строго десериализуется для `format_version = "0.1"`. Первый профиль
  реализует `[book]`, `[[spine]]`, `[features]` и `[[resources]]`; unknown fields
  являются ошибкой совместимости.
- Каждая spine-глава проходит через общий `compile_markdown` с dialect
  `lumi-markdown`. Importer снаружи объединяет units, строит chapter TOC,
  разрешает cross-file Markdown links и local raster images.
- Markdown source ranges преобразуются в `LumSourceLocator` с `book_id`,
  `chapter_id`, package path и heading identity. Reader, pagination, progress и
  annotations не получают LUM-specific ветку.
- `lum:*` и rich fences остаются typed safe placeholders. Required plugins
  отклоняются до появления first-party capability runtime; optional plugins
  дают diagnostics.
- Durable import использует общий source blob, job lifecycle, normalized
  package, resource manifests и source download.

## Лимиты первого профиля

- source: 100 MiB;
- entries: 10 000;
- expanded archive: 512 MiB;
- one resource: 64 MiB;
- `lum.toml`: 2 MiB;
- one chapter: 8 MiB;
- normalized blocks: 50 000;
- compression ratio: 100:1.

Отсутствующий или неканонический `mimetype` пока является warning. После
стабилизации packaging tools это правило можно повысить до error отдельным
versioned profile.

## Последствия

- Реализованы FMT-LUM-001 и portable часть FMT-LUM-002: package, manifest,
  spine, multi-chapter compilation, links, raster resources и stable source map.
- Source-folder import, `lum validate/pack/inspect`, backlinks/concept graph,
  semantic book roles и executable learning blocks остаются additive slices.
- Domain marker: `s1.2026-07-23.lum-import-v1`.
- Normalized package marker остаётся `normalized.reflowable.s1`.
