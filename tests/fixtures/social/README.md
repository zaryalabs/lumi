# Social contract fixtures

`contracts/v1/community-space.json` фиксирует безопасный E1 DTO и capability
boundary.

`contracts/v1/material-sharing.json` фиксирует безопасный E2 metadata/claim
DTO. Fixtures намеренно не содержат raw invite token, source blob, normalized
package, protected fingerprint, private annotation или progress.

`contracts/v1/material-discussions.json` фиксирует независимый E3 contract
material-level threads/replies, masked moderation projection и отсутствие
anchor/quote/private-record полей.

`contracts/v1/community-communications.json` фиксирует отдельные E4
chat/activity pages; activity не содержит message body или произвольный
payload.

Расширенный cross-revision backfill, shared highlights и anchor corpus появятся после
prerequisites, перечисленных в
`docs/tmp-plans/0.5.0-deferred-until-0.4.0.md`.
