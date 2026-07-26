# Автономное исполнение плана через Codex CLI

Статус: исполняемый

Этот runbook описывает ручной запуск
[`scripts/execute_plan.py`](../../scripts/execute_plan.py) внутри выделенного
devcontainer. Runner последовательно исполняет текущий
[`ROADMAP.md`](../tmp-plans/ROADMAP.md): два свежих вызова `codex exec`, внешний
quality gate и один commit на этап.

Runner не выполняет `push`, не открывает PR и не развёртывает внешнее
окружение.

## Контур безопасности

Codex запускается с
`--dangerously-bypass-approvals-and-sandbox`, поэтому runner намеренно
отказывается работать вне Lumi devcontainer.

Devcontainer:

- монтирует только этот repository и отдельный volume с Codex auth;
- не получает Docker socket хоста;
- использует отдельный Docker-in-Docker daemon для Compose, PostgreSQL и
  image-сборок;
- сохраняет логи/checkpoints только в ignored
  `.local/codex-plan-runs/`;
- запускает Codex непривилегированным пользователем `vscode`.

Docker-in-Docker service является privileged-контейнером и разделяет kernel с
Docker host. На macOS он дополнительно находится внутри VM Docker Desktop, но
не является строгой security boundary для недоверенного repository. Перед
запуском:

- проверьте текущий diff и инструкции repository;
- не передавайте в container SSH keys, cloud credentials и production
  secrets;
- используйте отдельный отзываемый Codex/API credential;
- не добавляйте host Docker socket в `.devcontainer/docker-compose.yml`.

Codex auth хранится в named volume `codex-home`. Сгенерированные agent commands
теоретически могут прочитать credential внутри container, поскольку
внутренний Codex sandbox отключён. После чувствительного или одноразового
прогона credential следует отозвать и удалить devcontainer volumes.

## Что делает один этап

Для каждого hardcoded stage:

1. Проверяет branch, `HEAD`, чистоту worktree, Git identity, Codex auth и
   совместимость CLI.
2. Запускает новую ephemeral Codex session для реализации только текущего
   этапа и сразу показывает её события в читаемом виде в текущем terminal.
3. Запускает вторую независимую ephemeral session для аудита diff и
   исправления пробелов, также с live output.
4. Проверяет, что Codex не изменил Git history, а подробный план и ROADMAP
   обновлены.
5. Повторно выполняет hardcoded gate без участия Codex.
6. Блокирует `.env`, private keys, `.local`, build output и другие
   чувствительные/generated paths, а также изменения runner/devcontainer
   control plane из продуктового этапа.
7. Выполняет `git diff --check`, staging и один commit с trailer
   `Lumi-Plan-Stage`.
8. Переходит к следующему этапу только после успешного commit.

Названия этапов, commit subjects и gate commands находятся вместе в начале
`scripts/execute_plan.py`. После изменения единого плана сначала синхронизируйте
этот список и выполните `make plan-runner-check`.

Runner требует, чтобы в каждом этапе менялись подробный plan и ROADMAP. Пока
активен запуск, нельзя переименовывать/удалять plan files и stage headings.
Архивацию завершённых tmp-plans выполняют отдельно после окончания выбранной
серии этапов.

## Подготовка devcontainer

На хосте нужны Docker Desktop и клиент, поддерживающий Dev Containers.

Перед первым открытием commit/stash существующую работу:

```sh
git status --short
```

Runner принципиально не смешивает уже существующий dirty worktree с первым
этапом.

В VS Code выполните `Dev Containers: Reopen in Container`. Через Make:

```sh
make devcontainer-up
```

Команда создаёт или запускает devcontainer и открывает внутри интерактивный
`bash`. После выхода из shell остановить и удалить его Compose-сервисы можно
с хоста:

```sh
make devcontainer-down
```

Named volumes `codex-home` и `docker-data` при этом сохраняются. Эквивалентный
ручной запуск через CLI:

```sh
devcontainer up --workspace-folder .
devcontainer exec --workspace-folder . bash
```

Первичная сборка устанавливает:

- последний опубликованный Codex CLI из npm tag `latest`;
- Rust `1.93.1`, `rustfmt`, `clippy` и wasm target из корневого
  `rust-toolchain.toml`;
- Dioxus CLI `0.7.9`;
- Node.js и Playwright Chromium image `1.57.0`;
- Docker CLI/Compose, PostgreSQL client, pre-commit и project dependencies.

Docker layer с `npm install` может быть взят из build cache. Поэтому
`postCreateCommand` дополнительно выполняет
`npm install --global @openai/codex@latest` при каждом создании devcontainer и
перед началом работы обновляет CLI по текущему npm tag. Уже запущенный
container сам по себе не обновляется; для ручного обновления без пересоздания:

```sh
sudo npm install --global @openai/codex@latest
codex --version
```

После открытия container авторизуйте Codex:

```sh
codex login --device-auth
codex login status
```

Настройте автора Git внутри repository, если локальная конфигурация отсутствует:

```sh
git config --local user.name "Your Name"
git config --local user.email "you@example.com"
```

Проверьте окружение:

```sh
docker info
make plan-runner-check
make plan-list
python3 scripts/execute_plan.py --dry-run --only 0.2.0/A1
```

## Запуск

Выполнить весь настроенный остаток roadmap:

```sh
python3 scripts/execute_plan.py
```

Начать с определённого этапа и продолжить дальше:

```sh
python3 scripts/execute_plan.py --from 0.2.0/A1
```

Проверить ровно один этап:

```sh
python3 scripts/execute_plan.py --only 0.2.0/A1
```

Продолжить с первого ещё не закоммиченного этапа до конца конкретного релиза:

```sh
python3 scripts/execute_plan.py --release 0.2.0
```

Граница текущего состояния определяется по commit trailer
`Lumi-Plan-Stage: <stage-id>`. Runner находит первый этап выбранного релиза без
такого commit и выполняет его вместе со всеми следующими этапами этого же
релиза. Более поздние релизы не выбираются. Если весь релиз уже закрыт, runner
завершается без Codex calls, gates и commits.

Безопасно проверить вычисленную границу можно через:

```sh
python3 scripts/execute_plan.py --dry-run --release 0.2.0
```

Для долгого прогона удобно использовать `tmux`:

```sh
tmux new -s lumi-plan
python3 scripts/execute_plan.py --from 0.2.0/A1
```

Отсоединение: `Ctrl-b`, затем `d`. Возврат:

```sh
tmux attach -t lumi-plan
```

`shutdownAction` devcontainer установлен в `none`, поэтому закрытие editor
само по себе не останавливает Compose project. Остановите его явно после
работы.

## Live-логи

Во время обоих `codex exec` runner сразу выводит в terminal:

- промежуточные сообщения Codex и reasoning summaries;
- каждую запускаемую команду;
- aggregated command output, status и exit code;
- ошибки, начало/завершение turn и token usage;
- неизвестные типы событий целиком, чтобы новая версия CLI не скрыла их.

Одновременно исходный поток каждого вызова без преобразований сохраняется как
валидный JSONL:

```text
.local/codex-plan-runs/<run-id>/<stage>/implementation.jsonl
.local/codex-plan-runs/<run-id>/<stage>/audit.jsonl
```

Рядом сохраняются `*.prompt.md`, `*.command.txt` и `*.last-message.md`.
Gate-команды также стримятся напрямую и сохраняются в `gate-*.log`.

## Остановка и продолжение

Перед каждым необратимым переходом runner атомарно обновляет:

```text
.local/codex-plan-runs/state.json
```

Prompts, сырые JSONL events, terminal-readable live output, последние ответы и
gate logs лежат в timestamped подкаталоге рядом.

После прерывания или исправления локальной инфраструктуры:

```sh
python3 scripts/execute_plan.py --resume
```

Поведение при ошибках:

- упавший первый/второй Codex вызов повторяется при resume;
- упавший внешний gate не создаёт commit и при resume запускается снова;
- runner не делает третий Codex repair-вызов;
- ручное изменение `HEAD` во время незавершённого этапа блокирует resume;
- уже созданный commit с правильным `Lumi-Plan-Stage` trailer распознаётся и
  не создаётся повторно.

После полного успеха активный checkpoint переносится в
`final-state.json` timestamped run directory.

## Модель и обслуживание manifest

По умолчанию модель берётся из Codex, а reasoning effort runner явно
устанавливает в `xhigh` для обоих вызовов каждого этапа. Для явного выбора
модели:

```sh
LUMI_CODEX_MODEL="<model-id>" python3 scripts/execute_plan.py --only 0.2.0/A1
```

Reasoning можно понизить для конкретного запуска:

```sh
LUMI_CODEX_REASONING_EFFORT=high \
  python3 scripts/execute_plan.py --release 0.2.0
```

Допустимые значения: `minimal`, `low`, `medium`, `high`, `xhigh`. Выбранная
модель должна поддерживать указанный effort. Эффективные model и reasoning
печатаются перед началом запуска и при `--dry-run`.

Не передавайте дополнительные CLI flags строкой из environment: runner
собирает аргументы без shell, чтобы избежать command injection.

После изменения ROADMAP:

1. Обновите `STAGES` и при необходимости наборы `*_GATE` в начале runner.
2. Сохраните уникальный Conventional Commit subject для каждого этапа.
3. Не используйте shell operators в gate commands; каждый command задаётся
   tuple аргументов.
4. Выполните:

   ```sh
   make plan-runner-check
   python3 scripts/execute_plan.py --dry-run
   ```

## Завершение и очистка

Посмотреть созданные commits:

```sh
git log --format='%h %s%n%b' --grep='Lumi-Plan-Stage'
```

Runner ничего не отправляет во внешний repository. Review и push выполняются
отдельно после просмотра истории и итогового `make c`.

Остановить devcontainer и удалить nested Docker/auth volumes:

```sh
docker compose -f .devcontainer/docker-compose.yml down --volumes
```

Удаление volumes необратимо удаляет сохранённую container-side Codex session
auth и nested Docker state.
