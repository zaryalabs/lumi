# Systems

Эта папка содержит техническое проектирование Lumi: как продуктовый vision из
[`../vision.md`](../vision.md) превращается в архитектуру, форматы данных,
интеграции и набор реализуемых подсистем.

Мы проектируем полную реализацию продукта, а не MVP и не временный срез.
Ранние срезы разработки определены отдельно в
[`../early-slices.md`](../early-slices.md).
Текущее состояние документов - **Final v01**: целевая архитектура принята для
планирования первого среза, а открытые вопросы считаются задачами прототипов,
ADR или реализации, но не блокируют сам target design.

## Цель

Зафиксировать для версии `v01` техническое видение Lumi по каждому крупному
направлению:

- что именно должна уметь функция;
- какие пользовательские сценарии она закрывает;
- какие данные и модели нужны;
- как функция встраивается в общий reader, синхронизацию, поиск, базу знаний,
  ИИ-слой и плагины;
- какие решения приняты, какие альтернативы отклонены, какие вопросы остаются
  открытыми.

## Процесс проектирования

Проектирование идет в несколько проходов.

1. **Pass 1: исходное видение.** Фиксируем первичную реализационную гипотезу по
   каждому направлению, без попытки сразу довести все до финальной архитектуры.
2. **Pass 2: согласование между подсистемами.** Возвращаемся к документам после
   проработки соседних частей и уточняем контракты, зависимости и ограничения.
3. **External review.** Сверяемся с видением внешнего эксперта, особенно там, где
   возможны более удачные архитектурные варианты.
4. **Final v01.** Закрепляем финальную версию проектирования для последующего
   планирования срезов и разработки.

Текущий проход завершает этап проектирования. Дальше работа должна идти через
выбор вертикальных срезов, ADR для спорных implementation choices и реализацию.

## Итог финального прохода

Финальная композиция Lumi строится вокруг нескольких сквозных контрактов:

- **Material -> DocumentRevision -> Normalized Content Package.** Все источники
  сначала превращаются в immutable revision и нормализованный package.
- **Reader-facing views.** Reflowable материалы открываются через
  `ReadingDocument`; PDF и fixed-layout EPUB - через `PageFidelityDocument`.
- **Anchor + provenance.** Заметки, хайлайты, поиск, learning, AI и social
  ссылаются на source-backed anchors, а не на DOM, пиксели или форматные
  offsets как единственный источник истины.
- **Draft-to-accepted flow.** AI, generated learning items, KB drafts and
  social publication не становятся сильными knowledge/search/social объектами
  без принятия пользователем или явной policy.
- **Cloud-backed web, full-copy native later.** Первый web target хранит
  состояние в облачной реплике аккаунта. Desktop/mobile проектируются как
  будущие full-copy replicas, а private/decentralized mode остается
  долгосрочным accepted requirement.
- **Shared spaces do not distribute private files.** Социальные функции
  синхронизируют comments, highlights, activity and material claims, но не
  раздают source blobs участникам без их собственной копии/прав.
- **One Job engine.** Imports, indexing, AI, transcription, exports, deletion
  workflows and anchor repair используют общий durable job/lifecycle contract.
- **Plugin platform as target, first-party first.** Плагины проектируются как
  полноценная extension platform, но ранние extension points должны
  валидироваться first-party plugins before broad third-party runtime.

## Статусы решений

В документах используем единые статусы:

- `draft` - первичная гипотеза, требует обсуждения;
- `accepted` - целевое решение принято для `v01`; открытые вопросы внутри
  документа остаются implementation/prototype questions;
- `revisit` - решение временно принято, но требует возврата после проработки
  связанных подсистем;
- `rejected` - вариант рассмотрен и отклонен с указанием причины;
- `open` - вопрос еще не решен.

## Структура

Каждое верхнеуровневое направление оформляется отдельным файлом. Вложенные
направления оформляются папкой с отдельными файлами внутри.

```text
docs/systems/
  README.md
  feature-registry.md
  normalized-content.md
  reading-screen.md
  reader-architecture.md
  backend-api.md
  security-privacy.md
  quality.md
  formats/
    epub.md
    fb2.md
    pdf.md
    web-reader.md
    telegram.md
    x.md
    markdown.md
    lum.md
  web-account.md
  sync.md
  knowledge-base.md
  obsidian.md
  search.md
  learning.md
  social.md
  ai.md
  plugins.md
```

## Направления

| Направление | Документ | Статус |
| --- | --- | --- |
| Регистр функций | `feature-registry.md` | `accepted` |
| Нормализованный контент | `normalized-content.md` | `accepted` |
| Экран чтения | `reading-screen.md` | `accepted` |
| Архитектура экрана чтения | `reader-architecture.md` | `accepted` |
| Backend и API boundaries | `backend-api.md` | `accepted` |
| Security и privacy | `security-privacy.md` | `accepted` |
| Quality, ADR и compatibility | `quality.md` | `accepted` |
| EPUB | `formats/epub.md` | `accepted` |
| FB2 | `formats/fb2.md` | `accepted` |
| PDF | `formats/pdf.md` | `accepted` |
| Веб-страницы в режиме чтения | `formats/web-reader.md` | `accepted` |
| Telegram через бота | `formats/telegram.md` | `accepted` |
| X: длинные посты и треды | `formats/x.md` | `accepted` |
| Markdown | `formats/markdown.md` | `accepted` |
| Собственный формат `lum` | `formats/lum.md` | `accepted` |
| Веб-аккаунт и облачная реплика | `web-account.md` | `accepted` |
| Синхронизация | `sync.md` | `accepted` |
| База знаний | `knowledge-base.md` | `accepted` |
| Интеграция с Obsidian | `obsidian.md` | `accepted` |
| Поиск | `search.md` | `accepted` |
| Механики обучения | `learning.md` | `accepted` |
| Социальные функции | `social.md` | `accepted` |
| ИИ-функционал | `ai.md` | `accepted` |
| Плагины | `plugins.md` | `accepted` |

## Регистр функций

[`feature-registry.md`](feature-registry.md) - поисковый индекс функций и
подсистем. Он нужен после реализации первого среза: по нему можно быстро найти
следующую user-visible функцию, понять ее зависимости и перейти к исходным
design docs.

Правило поддержки регистра: новая функция, крупная ADR или существенное
изменение scope должны обновлять соответствующую строку регистра или добавлять
новый stable feature id.

[`../early-slices.md`](../early-slices.md) задает первые implementation slices:
core architecture skeleton, web EPUB reader, macOS desktop reader and Android
reader. Эти срезы используют ID из регистра, но остаются отдельным документом,
потому что отвечают за порядок разработки, а не за полный функциональный
inventory.

## Композиционная модель

Функциональные направления делятся на четыре слоя:

- **Foundation layer.** Account, sync, blobs, jobs, security, normalized content,
  anchors and API boundaries.
- **Reading layer.** Library/import, reader, annotations, navigation,
  page/fidelity surfaces and reader timeline.
- **Knowledge layer.** Search, KB, learning and AI artifacts, all tied back to
  source refs.
- **Coordination/extension layer.** Social shared spaces, Obsidian projection,
  plugins, external agents and future private/decentralized mode.

При выборе первого или следующего среза лучше брать вертикальный пользовательский
workflow через несколько слоев, а не реализовывать слой целиком. Например:
`web account -> import -> normalized package -> reader -> annotation -> search
index`, после чего следующий срез расширяет тот же путь на KB, learning, AI или
social.

## Шаблон документа направления

Каждый документ проектирования держим в одинаковой форме, чтобы решения было
легко сравнивать и пересматривать.

```markdown
# Название направления

Status: draft

## Контекст

Что это направление должно дать продукту и почему оно важно.

## Пользовательские сценарии

- ...

## Функциональные требования

- ...

## Нефункциональные требования

- ...

## Модель данных

Какие сущности, идентификаторы, связи и метаданные нужны.

## Реализация

Основной подход, библиотеки, сервисы, фоновые задачи, клиентские и серверные
границы.

## Интеграции и зависимости

Связи с reader, форматами, синхронизацией, поиском, базой знаний, ИИ и
плагинами.

## Альтернативы

Какие варианты рассматривались и почему они хуже или лучше.

## Открытые вопросы

- ...
```

## Общие принципы

- Reader должен иметь унифицированную внутреннюю модель отображения, чтобы
  заметки, хайлайты, поиск, обучение и ИИ-функции работали поверх разных
  исходных форматов одинаково.
- Все импортеры должны создавать immutable `DocumentRevision` и внутренний
  Normalized Content Package. `ReadingDocument` и `PageFidelityDocument` являются
  reader-facing view models поверх этого пакета, а не исходным форматом
  хранения.
- Исходные материалы и пользовательские данные должны оставаться переносимыми:
  Lumi не должен запирать пользователя в закрытом хранилище без экспорта.
- Web-версия является cloud-backed web application: материалы, normalized
  packages, blobs, jobs and search indexes для web живут на сервере. Browser
  storage может использоваться только как неавторитетный cache.
- Desktop и mobile должны получить полноценные local replicas: локальное
  хранилище, локальные blobs/packages, outbox/sync и offline search.
- В долгосрочной архитектуре native clients должны поддерживать private /
  decentralized mode: пользователь может отключить web/cloud replica и хранить
  private vault только на своих устройствах, оставляя серверу только account,
  device registration, encrypted relay/key envelopes, social coordination and
  explicitly shared objects.
- Производные данные не являются источником истины: search indexes, thumbnails,
  page maps, backlinks, caches and calculated projections must be rebuildable.
- ИИ-слой должен быть заменяемым: пользователь может подключить свой ключ,
  использовать встроенную подписку или отключить ИИ-сценарии.
- Архитектура должна проектировать полноценную plugin platform уровня
  VS Code/Obsidian: manifest, activation events, commands, UI contributions,
  capabilities, plugin data, trust levels and marketplace path. Roadmap может
  отложить runtime/marketplace, но целевой design не должен быть урезан.
