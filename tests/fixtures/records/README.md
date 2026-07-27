# Records v2 fixtures

Открытый compatibility corpus для `lumi.annotations.v1` → Annotation v2:

- `v1-v2-annotations.json` содержит legacy same-block note без v2 metadata и
  новую Cyrillic/English margin note с title, tags и structural target.

Fixture читается unit-тестом `lumi-core`; PostgreSQL migration и browser E2E
дополняют его durable и rendering-проверками.
