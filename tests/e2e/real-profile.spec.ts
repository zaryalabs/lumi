import { expect, test } from "@playwright/test";

const realProfileEnabled = process.env.LUMI_E2E_REAL_PROFILE === "1";
const apiBase = process.env.LUMI_E2E_API_BASE ?? "http://127.0.0.1:8080/api/v1";

test.describe("real local profile", () => {
  test.skip(
    !realProfileEnabled,
    "set LUMI_E2E_REAL_PROFILE=1 to run against a live Lumi API and Web profile",
  );

  test("checks live API health and renders the web shell", async ({
    page,
    request,
  }) => {
    const response = await request.get(`${apiBase}/health`);
    expect(response.ok()).toBe(true);

    const health = (await response.json()) as {
      status?: string;
      service?: string;
      api_version?: string;
    };
    expect(health).toMatchObject({
      status: "ok",
      service: "lumi-server",
      api_version: "v1",
    });

    await page.goto("/");
    await expect(
      page.getByRole("heading", { name: "Reader platform adapter" }),
    ).toBeVisible();
    await expect(
      page.getByRole("complementary", { name: "Development status" }),
    ).toBeVisible();
  });
});
