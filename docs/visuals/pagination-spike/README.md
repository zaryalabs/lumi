# Pagination spike

Исполняемый browser-spike для `SPIKE-001`, `RD-003` и ADR 0006.

Он проверяет browser-measured pagination поверх source-backed fixture с
длинным текстом, Unicode grapheme sequences, изображением, таблицей, сноской и
plugin placeholder. `PageMap` хранит node ids и half-open Unicode scalar
ranges, а не DOM UTF-16 offsets, DOM paths или номера страниц.

Запуск:

```sh
make pagination-spike-r
make pagination-spike-e2e
```

Spike является временной проверкой алгоритма, а не production reader UI.
