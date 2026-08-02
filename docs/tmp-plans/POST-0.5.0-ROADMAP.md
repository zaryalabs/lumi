# Post-0.5.0 roadmap

Status: `completed`

Последнее обновление: 2026-08-02

## Назначение

После закрытого roadmap `0.2.0–0.5.0` первым bounded эпиком стал UI/UX и PWA
hardening Web-клиента. Он не добавляет новую domain vertical и сохраняет
non-goals предыдущих релизов.

## 1. Web UI/UX и PWA hardening

Подробный план:
[`ui-ux-pwa-improvement-plan.md`](ui-ux-pwa-improvement-plan.md).

- [x] semantic design tokens, primitives и автоматическая проверка CSS vars;
- [x] application shell v2, mobile navigation и Settings hierarchy;
- [x] typed origin, exact Search/social routes и исправленные learning/
  Community journeys;
- [x] Reader action hierarchy, overlay lifecycle, adaptive PageMap, bounded
  TOC и PDF selection parity;
- [x] installable PWA со static-only cache, offline fallback, controlled update
  и rollback runbook;
- [x] Chromium/WebKit platform matrix, обновлённый acceptance prototype,
  canonical docs и ADR 0041.

Результат: Web surfaces используют единый shell и visual contract, Reader
сохраняет semantic position при resize, а PWA не кеширует owner/API/source/
audio data. Следующие продуктовые функции требуют отдельного scope.
