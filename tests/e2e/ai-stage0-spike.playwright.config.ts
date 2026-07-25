import { defineConfig, devices } from "@playwright/test";

const port = process.env.LUMI_AI_SPIKE_PORT ?? "4174";
const baseURL = `http://127.0.0.1:${port}`;

export default defineConfig({
  testDir: ".",
  testMatch: "ai-stage0-spike.spec.ts",
  reporter: "list",
  use: {
    baseURL,
    trace: "on-first-retry",
  },
  webServer: {
    command: `python3 -m http.server ${port} --bind 127.0.0.1 --directory ../../spikes/ai-web`,
    reuseExistingServer: true,
    timeout: 30_000,
    url: baseURL,
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
    {
      name: "mobile-chromium",
      use: { ...devices["Pixel 7"] },
    },
  ],
});
