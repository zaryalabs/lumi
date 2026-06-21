# Интеграция с Obsidian

Status: accepted

## Контекст

Obsidian важен для Lumi по двум причинам:

- многие пользователи уже ведут личную базу знаний в Markdown vault;
- Vision явно требует links в стиле Obsidian, backlinks, graph и интеграцию с
  Obsidian.

Базовая интеграция должна идти через файловую систему: Markdown files, assets,
front matter и stable ids. Это лучше соответствует переносимости и не требует
зависимости от внутреннего API Obsidian. При этом Obsidian companion plugin
остается возможным future path для deep links, commands и richer sync.

Важно: Obsidian vault не становится primary database Lumi. Primary state живет в
Lumi local store и sync. Filesystem projection/import is an integration
boundary.

## Пользовательские сценарии

- Пользователь выбирает папку Obsidian vault на desktop.
- Lumi экспортирует highlights, notes и summaries в Markdown files inside
  vault.
- Пользователь редактирует экспортированную note в Obsidian. Lumi видит
  изменение, импортирует его и обновляет KB note.
- Пользователь создает note в Obsidian с `[[wikilinks]]`; Lumi добавляет ее
  в knowledge base и graph.
- Пользователь сохраняет source links из notes обратно к Lumi material anchors.
- Пользователь отключает интеграцию; Lumi оставляет readable Markdown files и
  stops watching folder.
- Web user exports vault bundle as zip, если direct filesystem watch недоступен.

## Функциональные требования

### Integration modes

Primary modes:

- **One-way export** - Lumi writes Markdown notes/annotations into selected
  vault/folder. Obsidian edits are ignored until explicit import.
- **Two-way folder sync** - Lumi watches selected folder and imports changes.
- **Manual import/export** - user imports vault folder/zip or exports KB as
  Markdown bundle.

Default для ранней реализации: one-way export + manual import. Two-way sync
нужен, но должен включаться явно, потому что conflicts и deletion semantics
сложнее.

### Filesystem projection

Рекомендуемая структура внутри vault:

```text
Obsidian Vault/
  Lumi/
    Notes/
      Idea.md
    Materials/
      Book Title.md
    Highlights/
      Book Title/
        Chapter 01.md
    AI/
      Summaries/
    Attachments/
    .lumi/
      manifest.json
      sync-state.json
      conflicts/
```

Папка может настраиваться, но Lumi должен по умолчанию писать в отдельный
namespace, чтобы не смешивать generated files with user vault unexpectedly.

### Front matter and ids

Каждый файл, которым управляет Lumi, получает front matter:

```yaml
---
lumi_id: kb_...
lumi_type: kb_note
lumi_revision: rev_...
title: "Idea"
created: "..."
updated: "..."
source:
  material_id: mat_...
  document_revision_id: docrev_...
  anchor_id: anch_...
---
```

Правила:

- `lumi_id` - primary binding между file and Lumi object.
- File path and title can change.
- Если `lumi_id` отсутствует, import treats file as external note.
- Если file contains unknown front matter, Lumi preserves it where possible.
- Lumi-owned metadata should live under `lumi_*` or nested `lumi:` namespace,
  чтобы не конфликтовать с user metadata.

### Markdown compatibility

Export должен использовать Obsidian-compatible Markdown:

- wikilinks `[[note]]`, `[[note#heading]]`, `[[note|alias]]`;
- embeds `![[asset.png]]`;
- tags in front matter and optional inline tags;
- callouts `> [!note]`;
- relative asset paths;
- readable quote blocks for highlights;
- deep links back to Lumi when platform supports them.

Для source anchors export uses readable fallback:

```markdown
> Quote from the material.

Source: [[Book Title]] · chapter 2 · page 31
Lumi: lumi://material/mat_.../anchor/anch_...
```

Если custom `lumi://` deep link не поддерживается, файл все равно полезен.

### Watching and import

Two-way sync pipeline:

1. Desktop filesystem watcher sees file create/update/delete/rename.
2. Lumi debounces and reads changed file.
3. Parser extracts front matter, body, links and attachments.
4. If `lumi_id` exists, update matching KB object.
5. If no `lumi_id`, create imported KB note with source `obsidian`.
6. If local Lumi object changed concurrently, create conflict object.
7. Update graph/search/sync.

Deletion policy:

- Deleted Obsidian file should not immediately delete Lumi object by default.
- Lumi marks projection missing and asks user if two-way delete should apply.
- Explicit "delete in both places" can create sync tombstone.

### Attachments

- Assets referenced by Markdown are copied or linked according to vault policy.
- Default: copy assets into configured `Attachments/` folder.
- Original material blobs are not copied into Obsidian automatically unless user
  exports material/package.
- Voice notes/transcripts can export as audio file + Markdown transcript.

### Conflict handling

Conflict sources:

- same note edited in Lumi and Obsidian;
- file renamed while note title changed in Lumi;
- deleted in Obsidian, edited in Lumi;
- front matter id duplicated by manual copy;
- attachment changed outside Lumi.

Conflict object must keep both versions. UI can offer:

- keep Lumi;
- keep Obsidian;
- merge manually;
- duplicate note.

## Нефункциональные требования

- **File safety.** Lumi must not rewrite entire vault or unknown files
  unexpectedly.
- **Human-readable output.** Exported Markdown should be useful without Lumi.
- **Explicitness.** Two-way sync and deletion propagation require opt-in.
- **Recoverability.** Before overwriting externally modified files, keep backup
  or conflict copy.
- **Cross-platform realism.** Desktop has best filesystem integration. Web uses
  File System Access API where available or manual import/export. Mobile may be
  export/import only.
- **Obsidian independence.** No hard runtime dependency on Obsidian internals.

## Модель данных

```text
ObsidianIntegration
  -> VaultBinding
  -> ProjectionRule[]
  -> FileBinding[]
  -> SyncState
  -> Conflict[]
```

Основные сущности:

- `ObsidianVaultBinding` - selected vault/folder, mode, platform permissions.
- `ObsidianProjectionRule` - mapping Lumi objects to paths/templates.
- `ObsidianFileBinding` - `lumi_id` <-> vault-relative path, hash, mtime.
- `ObsidianSyncState` - last imported/exported revision and file hash.
- `ObsidianImportIssue` - parse/duplicate/missing attachment issue.
- `ObsidianConflict` - conflicting Lumi and filesystem versions.

File binding:

```text
ObsidianFileBinding {
  id
  lumi_object_type
  lumi_object_id
  vault_id
  relative_path
  last_exported_revision_id
  last_seen_file_hash
  last_seen_mtime
  mode: export_only | two_way
}
```

## Реализация

### Desktop path

Desktop integration:

- user selects vault/folder;
- Lumi stores permission/path binding;
- file watcher observes changes;
- writes are atomic: temp file + rename where possible;
- path sanitizer prevents writing outside configured folder;
- `.lumi/sync-state.json` stores projection metadata for portability/debugging,
  while primary state remains in Lumi local store.

### Web path

Web integration options:

- File System Access API for supported browsers with explicit directory
  permission;
- manual export as zip;
- manual import from folder/zip;
- no background watch unless browser supports it safely.

Web should not pretend to have continuous Obsidian sync if browser permissions
do not allow it.

### Companion plugin

Obsidian plugin is `revisit`, not primary:

Potential benefits:

- command "Open in Lumi";
- better deep links;
- live status and conflict UI inside Obsidian;
- mobile Obsidian scenarios;
- controlled metadata edits.

Costs:

- separate TypeScript plugin project;
- plugin distribution and compatibility;
- dependency on Obsidian API;
- larger maintenance surface.

For `v01` filesystem integration gives most value with less coupling.

### Export templates

Lumi should support templates for:

- material summary note;
- per-highlight note;
- chapter summary;
- accepted KB note;
- flashcards/questions export;
- AI artifact export.

Templates must be data-only/configurable, not arbitrary code execution.

## Интеграции и зависимости

- **База знаний.** Obsidian integration projects KB notes and imports external
  Markdown into KB.
- **Reader.** Reader annotations and highlights export as Markdown with source
  anchors.
- **Синхронизация.** Filesystem changes become local KB changes, then sync via
  Lumi sync. Vault folder itself is not the sync log.
- **Поиск.** Imported/exported notes update unified index.
- **Learning.** Questions/flashcards can export to Markdown, but scheduling
  remains in Lumi.
- **ИИ.** AI artifacts can become Markdown files only after acceptance or
  export action.
- **Плагины.** Export templates and import adapters can become plugin extension
  points, but filesystem safety remains core.

## Альтернативы

- `accepted`: filesystem Markdown projection as primary integration. It is
  portable, inspectable and does not depend on Obsidian internals.
- `rejected`: Obsidian plugin as only integration path. It excludes users who
  only want files and makes Lumi depend on Obsidian API/distribution.
- `rejected`: writing directly into arbitrary existing vault structure by
  default. Too much risk of unwanted churn and conflicts.
- `rejected`: using Obsidian vault as primary database. Conflicts with web,
  mobile, sync, stable ids and non-note domain objects.
- `revisit`: Git-based vault sync. Useful for advanced users, but should sit
  outside core sync model.

## Открытые вопросы

- Какой exact default folder layout удобнее для Obsidian users?
- Делать ли one-file-per-highlight или grouped chapter/material notes by
  default?
- Нужен ли custom URI scheme `lumi://` in `v01`?
- Как глубоко поддерживать Obsidian-specific syntax beyond wikilinks, embeds
  and callouts?
- Когда стоит писать companion plugin?
