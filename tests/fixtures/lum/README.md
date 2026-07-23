# LUM fixtures

Корпус фиксирует portable `.lum` import без исполнения HTML/JS и без сетевых
ресурсов.

- `supported/` — source project для книги из двух Markdown-глав с
  cross-file heading link. Unit- и E2E-тесты упаковывают те же файлы в
  constrained ZIP во время выполнения.
- Небезопасные paths, неизвестные manifest fields, отсутствующие entries и
  лимиты контейнера строятся непосредственно в unit-тестах, чтобы явно
  контролировать повреждённую ZIP-структуру.
