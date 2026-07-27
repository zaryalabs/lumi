# AI fixtures

Frozen contract catalog for `0.2.0`.

- `contracts/v1/` фиксирует Rust-compatible task/context/event/artifact
  snapshots, полный HTTP route catalog и prompt/output schema registry
  Contract Freeze 1.
- `security/` фиксирует adversarial inputs, которые должны проходить через
  provider/context/artifact boundaries как данные, а не инструкции.

Core contract tests загружают snapshots напрямую. Любое breaking изменение
создает новый version directory и требует согласования треков A, B и C. Реальные
provider keys и private user content в fixtures запрещены.
