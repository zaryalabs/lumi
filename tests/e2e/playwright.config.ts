import { defineConfig, devices } from "@playwright/test";

const baseURL = process.env.PLAYWRIGHT_BASE_URL ?? "http://127.0.0.1:5173";
const postgresPort = process.env.LUMI_E2E_POSTGRES_PORT ?? "55432";

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
          command: `make -C ../.. db-up db-migrate server-r LUMI_POSTGRES_PORT=${postgresPort}`,
          reuseExistingServer: true,
          timeout: 120_000,
          url: "http://127.0.0.1:8080/api/v1/ready",
        },
        {
          command: "make -C ../.. web-r",
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
