# Плагины

Status: draft

## Контекст

Плагины нужны, чтобы Lumi можно было расширять без переписывания ядра:

- новые источники материалов;
- новые форматы импорта;
- extraction/post-processing;
- reader interactive blocks;
- export/import integrations;
- AI providers и task kinds;
- learning exercise types;
- KB graph enrichers;
- search extractors/fields.

Модель должна быть ближе к VS Code/Obsidian по продуктовой форме: installable
extensions with manifests, activation events, commands, settings и declared
capabilities. Но технически Lumi не должен давать плагинам произвольный доступ
к процессу, файлам и user data. Core data model, sync, anchors and security
boundaries remain owned by Lumi.

## Пользовательские сценарии

- Пользователь включает bundled first-party plugin for Mermaid, math или code
  highlighting.
- Пользователь устанавливает plugin for a new source provider.
- Plugin adds "Import from ..." command.
- `lum` package requires `lumi.quiz` and optional `lumi.mermaid`; Lumi checks
  plugin availability.
- Plugin renders interactive reader block inside sandboxed surface.
- Plugin adds AI provider compatible with local model или external service.
- User reviews permissions before enabling plugin.
- Broken plugin fails gracefully и не corrupt core library.

## Функциональные требования

### Plugin categories

Supported extension points:

- **Source provider** - fetch/import from external service.
- **Format importer** - convert source file/payload to `ReadingDocument`.
- **Post-processor** - OCR, text cleanup, entity extraction, metadata enrich.
- **Reader block** - render typed interactive block.
- **Reader action** - command on selection/material.
- **AI provider/task** - provider client или new task kind/schema.
- **Learning plugin** - exercise type, scheduler, validator.
- **KB plugin** - note action, graph edge generator, export template.
- **Search plugin** - text extractor, field enricher, custom analyzer.
- **Export plugin** - output to Markdown/HTML/Anki/etc.

Not all extension points need third-party support in `v01`. First-party plugins
should use the same contracts where practical.

### Manifest

Each plugin has manifest:

```toml
id = "example.plugin"
name = "Example Plugin"
version = "0.1.0"
publisher = "example"
min_lumi_version = "0.1.0"

[activation]
events = ["onCommand:example.import", "onBlock:example.block"]

[capabilities]
filesystem = ["read_selected_files"]
network = ["api.example.com"]
sync = ["plugin_objects"]
reader = ["render_block", "selection_action"]
search = ["provide_text"]
ai = ["provider"]
```

Manifest responsibilities:

- identity и version;
- entrypoints;
- extension points;
- capabilities и permissions;
- settings schema;
- optional dependencies;
- supported platforms;
- plugin data schema versions.

### Capability model

Plugins get no ambient authority. Capabilities are explicit:

- filesystem read/write scopes;
- network domains;
- access to selected material text;
- access to KB notes;
- ability to create AI tasks/artifacts;
- ability to add sync objects;
- ability to render reader blocks;
- ability to run external command;
- ability to store plugin data.

User must approve dangerous capabilities, especially:

- broad filesystem;
- network;
- external command;
- access to personal notes/library;
- social/shared folder access.

### Runtime model

Draft runtime split:

- **First-party bundled plugins** can be Rust/native modules compiled with Lumi
  or loaded as trusted packages.
- **Third-party portable plugins** should prefer WebAssembly/WASI for processing
  and sandboxed web components/iframes for UI blocks.
- **Desktop-only plugins** may use external command/process integration with
  explicit permission.
- **Web plugins** cannot assume local process or arbitrary filesystem access.

This gives VS Code/Obsidian-like extension UX while keeping cross-platform and
security constraints realistic.

### Reader blocks

Reader block plugin:

- receives typed input from `ReadingDocument` or `lum` block;
- does not receive raw file access unless capability allows;
- returns render descriptor or mounts sandboxed UI;
- provides measurement hints for pagination;
- provides anchor mapping for interactions where needed;
- supports offline rendering if resources are local;
- reports errors as placeholders.

Examples:

- math/LaTeX;
- Mermaid;
- syntax highlighting;
- quiz/flashcard block;
- data visualization;
- code example runner as future high-risk plugin.

### Plugin data

Plugins can store:

- settings;
- cache;
- plugin-owned objects;
- generated artifacts;
- local secrets through secure storage references.

Plugin data must be namespaced by plugin id. Sync requires declared schema and
capability. Uninstalling plugin should not delete user data without explicit
confirmation.

### Versioning

- Plugin API has semver.
- Plugin manifest declares compatible Lumi versions.
- `lum` packages can declare required/optional plugins.
- If required plugin missing, material enters blocked/degraded state.
- If optional plugin missing, reader shows placeholder or fallback.
- Plugin data migrations must be explicit.

### Marketplace and trust

Full marketplace can come later, but design needs trust levels:

- `first_party` - shipped with Lumi.
- `verified` - signed/reviewed.
- `community` - user-installed.
- `dev` - local development.

Install UI should show trust level, capabilities, platform support and update
source.

## Нефункциональные требования

- **Safety.** Plugin cannot corrupt core database or read private data without
  permission.
- **Portability.** Core extension contracts are platform-neutral where possible.
- **Graceful degradation.** Missing/broken plugin yields placeholder, not broken
  reader.
- **Observability.** Plugin errors are visible in diagnostics.
- **Determinism.** Importer/postprocessor plugins should produce reproducible
  outputs given same input and version where possible.
- **Performance.** Plugins should have time/memory budgets for indexing,
  rendering and processing.
- **Reviewability.** Manifest makes data/network/file access visible.

## Модель данных

```text
PluginRegistry
  -> PluginPackage[]
  -> PluginInstallation[]
  -> CapabilityGrant[]
  -> PluginRuntime[]
  -> PluginObject[]
```

Основные сущности:

- `PluginManifest` - declared metadata/capabilities/entrypoints.
- `PluginPackage` - installed package, source, checksum, trust level.
- `PluginInstallation` - enabled/disabled state per user/device.
- `CapabilityGrant` - approved permission scope.
- `PluginActivation` - activation event and runtime instance.
- `PluginCommand` - command exposed to UI.
- `PluginBlockRenderer` - renderer for typed reader block.
- `PluginTask` - background job owned by plugin.
- `PluginObject` - plugin-owned data object.
- `PluginDiagnostic` - warnings/errors/performance issues.

Plugin object:

```text
PluginObject {
  id
  plugin_id
  object_type
  schema_version
  payload
  sync_policy
  created_at
  updated_at
}
```

## Реализация

### Extension host

Core services expose narrow APIs:

- read selected source context;
- create material/import result;
- create annotation/KB note/artifact through validated commands;
- register block renderer;
- register provider/importer;
- enqueue background task;
- read/write plugin data;
- emit diagnostics.

Plugins should not get direct SQL access. They call typed host APIs.

### WASM processing plugins

Candidate for:

- importers;
- text extractors;
- metadata parsers;
- post-processing;
- search field enrichers.

WASM host provides:

- input payload/resource handles;
- limited memory/time;
- no network unless proxied through capability API;
- output schema validation.

### UI plugins

Reader/UI plugins can render in sandbox:

- iframe/webview-like surface for web/desktop;
- declarative render descriptors for simple blocks;
- native fallback placeholders for unsupported platforms.

Plugin UI cannot directly manipulate reader DOM/state. It sends events through
host API.

### External command plugins

Desktop-only high-risk plugin type:

- command and args declared in manifest/settings;
- explicit user approval;
- no background arbitrary execution without activation event;
- stdout/stderr/result parsed through schema;
- useful for local OCR, custom agents, converters.

This should not be the default plugin model for cross-platform features.

### First-party plugins

These can be implemented first using same contracts:

- `lumi.math`;
- `lumi.mermaid`;
- `lumi.code`;
- `lumi.svg`;
- `lumi.quiz`;
- `lumi.flashcard`;
- OCR/text extraction helpers;
- OpenRouter AI provider.

Using first-party plugins validates the extension API before third-party
marketplace.

## Интеграции и зависимости

- **Reader.** Plugins render typed blocks and add selection/material actions,
  but anchors/annotations remain core.
- **Форматы.** Format plugins output `ReadingDocument` and diagnostics.
- **Синхронизация.** Plugin objects sync only with declared schemas and grants.
- **Search.** Plugins can provide text/extracted fields, not bypass permission
  filters.
- **База знаний.** Plugins may add note actions/exporters/graph enrichers.
- **Obsidian.** Export templates/import helpers can become plugins.
- **Learning.** Exercise types and schedulers can be plugins after core types.
- **ИИ.** Providers and task kinds are plugin extension points.
- **Social.** Social plugins require explicit shared-space capabilities.

## Альтернативы

- `accepted`: VS Code/Obsidian-like manifest + activation + capabilities model.
- `accepted`: first-party plugins use same contracts before third-party rollout.
- `rejected`: arbitrary JS plugins with full app access like classic Obsidian
  community plugins. Too risky for cross-platform, sync and private libraries.
- `rejected`: native dynamic libraries as default. Too unsafe and platform
  specific.
- `rejected`: no plugins, only hardcoded features. This blocks new sources,
  formats and AI providers.
- `revisit`: marketplace and signed packages. Needed later, but not necessary
  for first contract design.
- `revisit`: TypeScript extension host. Familiar for plugin authors, but
  conflicts with Rust/core/WASM portability unless carefully sandboxed.

## Открытые вопросы

- What exact runtime should third-party plugins use first: WASM-only,
  TypeScript sandbox, or hybrid?
- How rich should declarative UI descriptors be before allowing iframe UI?
- Which plugin capabilities are safe enough for mobile?
- How to sign/verify community packages?
- Should plugin API be stable in `v01` or explicitly experimental until first
  first-party plugins settle?
