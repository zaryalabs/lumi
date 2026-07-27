# ADR 0032: Stable LinkTarget, wikilinks и backlinks

Status: accepted

## Контекст

Пользователь вводит читаемые `[[wikilinks]]` в Markdown-заметках, но title и
структурный путь могут измениться. Сохранение только текста делает rename
разрушающим; автоматический выбор первого одноимённого объекта ведёт к
непредсказуемой навигации. Links нужны Reader и будущему Desk до полноценной
Knowledge Base и не должны зависеть от Obsidian.

## Решение

1. Markdown body остаётся source of truth для читаемого ввода. Общий безопасный
   extractor выделяет `[[target]]`, `[[target#heading]]` и
   `[[target|alias]]`, сохраняет raw token и не выполняет HTML/JS.
2. После каждой create/update note сервер в той же транзакции перестраивает
   `annotation_links`. Delete annotation удаляет исходящие link rows; primary
   note tombstone сохраняется.
3. `LinkTarget` содержит `object_type`, stable `object_id`, optional
   `material_id`/source-backed `anchor` и rebuildable `display_path`.
   `0.4.0/E2` поддерживает `material`, `annotation` и `anchor`.
4. Resolver сравнивает доступные объекты account scope. Current material влияет
   только на порядок suggestions. При двух точных совпадениях state —
   `ambiguous`; silent first-match запрещён. При отсутствии цели state —
   `unresolved`.
5. Explicit `POST /api/v1/links/resolve` принимает только текущего доступного
   кандидата и сохраняет stable target. Переименование меняет display
   projection, но не target id.
6. Backlinks — rebuildable query projection над resolved links. Циклы допустимы:
   list/detail rendering не выполняет рекурсивный обход graph.
7. Heading target связывается с immutable revision id и полным `Anchor`.
   Неразрешённый heading не деградирует молча до material.
8. Suggestions, resolution, backlinks и material link listing всегда начинают
   с owner authorization. Foreign target выглядит как not found.
9. Export включает outgoing links и incoming backlinks; unresolved raw input
   сохраняется.

## Последствия

- Rename не ломает уже разрешённую ссылку.
- Неоднозначность видна пользователю и требует выбора.
- Backlinks можно полностью перестроить из annotation bodies и bindings.
- Будущие `learning_item`, `ai_artifact` и `kb_note` расширят target type без
  изменения существующих ids.

## Альтернативы

- `rejected`: хранить только display path — rename ломает связь.
- `rejected`: выбирать первый exact match — результат зависит от порядка SQL.
- `rejected`: автоматически создавать KB note для unknown name — KB не входит
  в этот эпик.
- `rejected`: использовать DOM path как anchor — несовместимо с source-backed
  anchor contract.

## Совместимость

- `annotation_links` — additive derived table; Annotation v1/v2 JSON остаётся
  читаемым.
- Markdown body не переписывается при resolution.
- Required tests: Cyrillic/English parse, alias/heading, repeated names,
  unresolved/ambiguous, explicit resolve, rename projection, cycle, owner
  isolation, export/backlinks и malicious HTML.

