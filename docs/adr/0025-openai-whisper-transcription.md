# ADR 0025: транскрибация через OpenAI Whisper API

Status: accepted

## Контекст

`AI-006` требует единого встроенного пути транскрибации для голосовых ответов
и voice notes. Общий AI-контур ориентирован на OpenRouter через
OpenAI-compatible API, но OpenRouter credential и chat provider contract не
определяют надежный доступ к аудиотранскрибации. Оставлять выбор speech-to-text
provider на момент реализации означало бы получить разные результаты,
ограничения и обработку ошибок в learning и Desk.

## Решение

- Встроенная транскрибация Lumi использует OpenAI Audio Transcriptions API:
  `POST /v1/audio/transcriptions`.
- Первичная модель — OpenAI Whisper с API model id `whisper-1`.
- Вызов выполняет server-side worker как durable `AiTask`/`Job`; browser не
  отправляет аудио в OpenAI напрямую.
- Для транскрибации нужен отдельный account-scoped OpenAI API credential.
  OpenRouter key не переиспользуется и не считается достаточным.
- Исходный `AudioAttachment` читается через общий авторизованный attachment
  contract. Worker передает OpenAI только аудиофайл и явно разрешенные
  параметры транскрибации.
- Результат сохраняется как versioned `TranscriptArtifact` с provenance:
  provider `openai`, model `whisper-1`, source attachment id, язык и доступные
  timing/diagnostic metadata. Пользовательская правка создает отдельную
  revision и не перезаписывает исходный результат.
- Внешний MCP-агент сохраняет возможность выполнить совместимую queued task,
  но такой результат не называется встроенной OpenAI/Whisper-транскрибацией,
  если provenance не подтверждает этот provider и model.
- Model id остается явной конфигурацией adapter и записывается в `AiRun`.
  Замена Whisper на другую модель или provider требует отдельного решения и
  compatibility/quality проверки, а не тихой смены backend.

## Последствия

- Learning и Desk получают один предсказуемый встроенный speech-to-text path.
- Пользователю с OpenRouter BYOK потребуется дополнительно настроить OpenAI API
  key для встроенной транскрибации.
- Аудио покидает Lumi и передается OpenAI; UI обязан показать это до запуска и
  применять общие privacy/retention controls.
- Ограничения форматов, размера и duration проверяются до provider call;
  provider errors проходят redaction и сохраняются как безопасные diagnostics.
- Offline/local transcription и realtime streaming не входят в это решение.

## Альтернативы

- Использовать OpenRouter/OpenAI-compatible chat provider как неявный
  speech-to-text backend: отклонено, потому что chat capability не гарантирует
  Audio Transcriptions API и Whisper.
- Выбирать speech-to-text provider автоматически: отклонено из-за
  непредсказуемого качества, стоимости и provenance.
- Транскрибировать в browser: отклонено для cloud-backed Web из-за secret
  handling, durability и retry.
- Встроить локальный Whisper runtime: отложено до отдельного offline/native
  решения.

## Совместимость и проверки

- Existing `AudioAttachment`, `AiTask`, `Job`, `AiRun` и
  `TranscriptArtifact` contracts сохраняются.
- Required tests: отсутствие OpenAI credential, unsupported MIME/oversize,
  timeout/retry/cancel, provider error redaction, duplicate completion,
  provenance `openai` + `whisper-1`, сохранение edited revision и удаление
  audio согласно retention policy.
