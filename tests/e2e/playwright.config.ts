import { defineConfig, devices } from "@playwright/test";

const webPort = process.env.LUMI_E2E_WEB_PORT ?? "5173";
const baseURL =
  process.env.PLAYWRIGHT_BASE_URL ?? `http://127.0.0.1:${webPort}`;
const postgresPort = process.env.LUMI_E2E_POSTGRES_PORT ?? "55432";
const apiPort = process.env.LUMI_E2E_API_PORT ?? "8080";
const openRouterPort = process.env.LUMI_E2E_OPENROUTER_PORT ?? "19090";
const apiBase = `http://127.0.0.1:${apiPort}/api/v1`;
const webOrigin = `http://127.0.0.1:${webPort}`;
const openRouterOrigin = `http://127.0.0.1:${openRouterPort}`;

export default defineConfig({
  testDir: ".",
  workers: process.env.CI ? 1 : undefined,
  testIgnore: [
    "prototype.spec.ts",
    "pagination-spike.spec.ts",
    "ai-stage0-spike.spec.ts",
  ],
  reporter: "list",
  use: {
    baseURL,
    trace: "on-first-retry",
  },
  webServer: process.env.PLAYWRIGHT_BASE_URL
    ? undefined
    : [
        {
          command: `LUMI_WEB_FIXTURE_ROOT=tests/fixtures/web LUMI_WEB_ORIGIN=${webOrigin} LUMI_AUTH_AUDIENCE=${webOrigin} LUMI_OPENROUTER_ENDPOINT=${openRouterOrigin}/api/v1/chat/completions LUMI_OPENAI_TRANSCRIPTION_ENDPOINT=${openRouterOrigin}/v1/audio/transcriptions make -C ../.. db-up db-migrate server-r LUMI_POSTGRES_PORT=${postgresPort} LUMI_SERVER_BIND=127.0.0.1:${apiPort}`,
          reuseExistingServer: true,
          timeout: 120_000,
          url: `${apiBase}/ready`,
        },
        {
          command: `make -C ../.. web-r LUMI_API_BASE=${apiBase} LUMI_WEB_PORT=${webPort}`,
          reuseExistingServer: true,
          timeout: 120_000,
          // The generated wasm exists only after the current Dioxus build is
          // ready; the source PDF.js asset is available too early.
          url: `${baseURL}/wasm/lumi-web_bg.wasm`,
        },
        {
          command: `LUMI_E2E_OPENROUTER_PORT=${openRouterPort} node ./openrouter-mock.mjs`,
          reuseExistingServer: true,
          timeout: 30_000,
          url: `${openRouterOrigin}/health`,
        },
      ],
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
});
