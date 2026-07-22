import { defineConfig, devices } from "@playwright/test";

const baseURL = process.env.PLAYWRIGHT_BASE_URL ?? "http://127.0.0.1:5173";
const postgresPort = process.env.LUMI_E2E_POSTGRES_PORT ?? "55432";
const apiPort = process.env.LUMI_E2E_API_PORT ?? "8080";
const apiBase = `http://127.0.0.1:${apiPort}/api/v1`;

export default defineConfig({
  testDir: ".",
  testIgnore: ["prototype.spec.ts", "pagination-spike.spec.ts"],
  reporter: "list",
  use: {
    baseURL,
    trace: "on-first-retry",
  },
  webServer: process.env.PLAYWRIGHT_BASE_URL
    ? undefined
    : [
        {
          command: `LUMI_WEB_FIXTURE_ROOT=tests/fixtures/web make -C ../.. db-up db-migrate server-r LUMI_POSTGRES_PORT=${postgresPort} LUMI_SERVER_BIND=127.0.0.1:${apiPort}`,
          reuseExistingServer: true,
          timeout: 120_000,
          url: `${apiBase}/ready`,
        },
        {
          command: `make -C ../.. web-r LUMI_API_BASE=${apiBase}`,
          reuseExistingServer: true,
          timeout: 120_000,
          url: baseURL,
        },
      ],
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
});
