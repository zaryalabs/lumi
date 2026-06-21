import { expect, test } from "@playwright/test";

test("renders the accessible Lumi S1 web reader", async ({ page }) => {
  await page.goto("/");

  await expect(
    page.getByRole("main", { name: "Lumi S1 web EPUB reader" }),
  ).toBeVisible();
  await expect(page.getByRole("heading", { name: "Lumi" })).toBeVisible();
  await expect(
    page.getByRole("navigation", { name: "Primary navigation" }),
  ).toBeVisible();
  await expect(page.getByRole("region", { name: "Library" })).toBeVisible();
  await expect(
    page.getByRole("region", { name: "Reader", exact: true }),
  ).toBeVisible();
  await expect(
    page
      .getByRole("region", { name: "Reader", exact: true })
      .getByRole("heading", { name: "Architecture Notes for Readers" }),
  ).toBeVisible();
  await expect(
    page.getByRole("region", { name: "Notes and highlights" }),
  ).toBeVisible();
  await expect(
    page.getByRole("region", { name: "Import diagnostics" }),
  ).toBeVisible();
  await expect(page.getByText("API v1")).toBeVisible();
});
