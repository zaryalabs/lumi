# PDF import и page-fidelity reader

Status: active

Runbook описывает загрузку PDF, server-side inspection и Web reader на PDF.js.

## Зависимости и запуск

Docker images уже устанавливают Poppler и собирают pinned PDF.js assets:

```sh
make up
```

Для host-native запуска нужны `pdfinfo`, `pdftotext` и `pdftoppm` из
`poppler-utils`. После установки:

```sh
make db-up
make db-migrate
make server-r
make web-r
```

`make web-r` и `make web-build` выполняют `make pdfjs-assets`. Команда
устанавливает точную версию из `apps/web/package-lock.json` через `npm ci`,
если `node_modules/pdfjs-dist` отсутствует, и копирует runtime, worker, CMaps и
standard fonts в игнорируемый каталог `apps/web/assets/vendor/pdfjs`.

Пути к Poppler можно задать явно:

```sh
LUMI_PDFINFO_BIN=/absolute/path/pdfinfo \
LUMI_PDFTOTEXT_BIN=/absolute/path/pdftotext \
LUMI_PDFTOPPM_BIN=/absolute/path/pdftoppm \
make server-r
```

## HTTP contract

Upload использует общий multipart endpoint:

```text
POST /api/v1/imports
Content-Type: multipart/form-data
Idempotency-Key: <1..200 chars>
X-Lumi-CSRF: <session csrf>

file=<document.pdf>
```

Основные read routes:

- `GET /api/v1/materials/{material_id}` — format/status/active revision;
- `GET /api/v1/revisions/{revision_id}` — immutable revision;
- `GET /api/v1/revisions/{revision_id}/package` — fixed-layout package;
- `GET /api/v1/revisions/{revision_id}/page-fidelity-document` — reader view;
- `GET /api/v1/materials/{material_id}/source` — исходный PDF, включая
  `Range: bytes=...`;
- общие `/progress` и `/annotations` — page-backed reader state.

Range endpoint принимает один byte range, возвращает `206`,
`Accept-Ranges: bytes`, `Content-Range` и точный `Content-Length`. Несколько
ranges и границы вне файла возвращают `416`.

## Lifecycle и diagnostics

```text
queued/source_accepted
  -> running/inspecting_document
  -> running/normalizing
  -> running/persisting
  -> succeeded/committed
```

Основные diagnostic codes:

- `pdf_invalid_header`, `pdf_malformed`, `pdf_engine_timeout`;
- `pdf_password_required`, `pdf_unsupported_drm`;
- `pdf_page_count_exceeded`, `pdf_page_geometry_exceeded`,
  `pdf_page_geometry_invalid`;
- `pdf_text_layer_exceeded`, `pdf_normalization_failed`;
- warnings `pdf_text_extraction_failed`, `pdf_thumbnail_failed`,
  `pdf_page_geometry_partial`, `pdf_ocr_candidate`.

Пароль пока нельзя передать через UI: locked PDF остаётся failed material с
diagnostic. OCR не запускается автоматически.

## Проверка

Пересоздать детерминированную фикстуру:

```sh
python3 scripts/generate_pdf_fixtures.py
```

Проверить структуру и визуальный рендер:

```sh
pdfinfo tests/fixtures/pdf/text-layer.pdf
pdftoppm -png tests/fixtures/pdf/text-layer.pdf /tmp/lumi-pdf-page
```

Автоматические gates:

```sh
make c
make web-e2e
```

`make web-e2e` использует disposable containerized Poppler, поэтому полный PDF
flow проходит и на host без установленных `pdfinfo`, `pdftotext` и
`pdftoppm`.

Ручной smoke flow: создать аккаунт, выбрать вкладку PDF, загрузить
`tests/fixtures/pdf/text-layer.pdf`, дождаться `Готово`, открыть материал,
проверить две страницы разной ориентации, zoom, selection, highlight, reload и
восстановление последней страницы.
