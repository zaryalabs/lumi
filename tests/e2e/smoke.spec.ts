import { expect, test } from "@playwright/test";

test("renders the accessible Lumi web shell", async ({ page }) => {
  await page.goto("/");

  await expect(
    page.getByRole("main", { name: "Lumi development shell" }),
  ).toBeVisible();
  await expect(page.getByRole("heading", { name: "Lumi" })).toBeVisible();
  await expect(
    page.getByRole("navigation", { name: "Primary navigation" }),
  ).toBeVisible();
  await expect(
    page.getByRole("region", { name: "Reader contract" }),
  ).toBeVisible();
  await expect(page.getByText("API v1")).toBeVisible();
});
