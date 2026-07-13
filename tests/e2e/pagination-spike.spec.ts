import { expect, test } from "@playwright/test";

type SpikeSnapshot = {
  coverageValid: boolean;
  generation: number;
  layoutKey: string;
  pageCount: number;
  kinds: string[];
};

async function snapshot(page: import("@playwright/test").Page) {
  return page.evaluate<SpikeSnapshot>(() => {
    const spike = window.paginationSpike;
    return {
      coverageValid: spike.coverageValid,
      generation: spike.generation,
      layoutKey: spike.layoutKey,
      pageCount: spike.pages.length,
      kinds: [
        ...new Set(
          spike.pages.flatMap((item) =>
            item.fragments.map((fragment) => fragment.kind),
          ),
        ),
      ],
    };
  });
}

test("builds a continuous PageMap across text, media, footnote and plugin block", async ({
  page,
}) => {
  await page.goto("/");
  await expect(page.locator("body")).toHaveAttribute("data-ready", "true");

  const result = await snapshot(page);

  expect(result.coverageValid).toBe(true);
  expect(result.pageCount).toBeGreaterThan(3);
  expect(result.kinds).toEqual(
    expect.arrayContaining([
      "paragraph",
      "figure",
      "table",
      "footnote",
      "plugin",
    ]),
  );
  await expect(page.locator("#coverage-status")).toHaveText("PageMap валиден");
});

test("rebuilds the PageMap after layout settings change", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("body")).toHaveAttribute("data-ready", "true");
  const before = await snapshot(page);

  await page.getByRole("slider", { name: "Размер текста" }).fill("22");
  await page
    .getByRole("combobox", { name: "Ширина страницы" })
    .selectOption("520");
  await expect
    .poll(async () => (await snapshot(page)).generation)
    .toBeGreaterThan(before.generation);
  const after = await snapshot(page);

  expect(after.coverageValid).toBe(true);
  expect(after.layoutKey).not.toBe(before.layoutKey);
  expect(after.pageCount).toBeGreaterThan(before.pageCount);
});

test("keeps page navigation semantic and bounded", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("body")).toHaveAttribute("data-ready", "true");
  const counter = page.getByTestId("page-counter");

  await expect(counter).toContainText("1 / ");
  await page.getByRole("button", { name: "Следующая страница" }).click();

  await expect(counter).toContainText("2 / ");
  await expect(
    page.getByRole("region", { name: "Измеренная страница" }),
  ).toBeVisible();
});

declare global {
  interface Window {
    paginationSpike: {
      coverageValid: boolean;
      generation: number;
      layoutKey: string;
      pages: Array<{
        fragments: Array<{ kind: string }>;
      }>;
    };
  }
}
