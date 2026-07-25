# AI fixtures

Stage 0 catalog for `0.2.0`.

- `contracts/v1/` фиксирует decision-level JSON snapshots до Contract Freeze 1.
  Они задают обязательные поля и fencing/version markers; Rust DTO и generated
  schemas добавляются на этапе 1 без неявного изменения этих примеров.
- `security/` фиксирует adversarial inputs, которые должны проходить через
  provider/context/artifact boundaries как данные, а не инструкции.

Любое breaking изменение создает новый version directory. Реальные provider
keys и private user content в fixtures запрещены.
