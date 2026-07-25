import { expect, test } from "@playwright/test";

declare global {
  interface Window {
    lumiDeepChatSpike: {
      attachSelection(): void;
    };
  }
}

test("Deep Chat remains a replaceable server-state adapter", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "chromium");
  await page.goto("/");
  const chat = page.locator("deep-chat");

  await expect(chat).toHaveAttribute("aria-label", "ИИ-чат Lumi");
  await expect(page.locator("body")).toHaveAttribute("data-rendered", "true");
  await expect(chat).toContainText("История загружена с сервера.");

  await page.evaluate(() => {
    window.lumiDeepChatSpike.attachSelection();
  });
  await expect(chat).toContainText("явно выбранный фрагмент");

  const input = chat.locator("#text-input");
  await input.fill("Объясни проще");
  await input.press("Enter");
  await expect(page.locator("body")).toHaveAttribute("data-stream", "complete");
  await expect(chat).toContainText("Потоковый ответ с цитатой [1].");

  await page.reload();
  await expect(chat).toContainText("Объясни проще");
});

test("Deep Chat stop event cancels the adapter stream", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "chromium");
  await page.goto("/");
  const chat = page.locator("deep-chat");
  const input = chat.locator("#text-input");

  await input.fill("Останови ответ");
  await input.press("Enter");
  await chat.locator("#stop-icon").evaluate((icon) => {
    (icon.parentElement as HTMLElement | null)?.click();
  });

  await expect(page.locator("body")).toHaveAttribute("data-stream", "stopped");
});

test("Deep Chat fits the mobile viewport and exposes a focusable composer", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "mobile-chromium");
  await page.goto("/");
  const chat = page.locator("deep-chat");
  const input = chat.locator("#text-input");

  await input.focus();
  await expect(input).toBeFocused();
  const box = await chat.boundingBox();

  expect(box?.width ?? Number.POSITIVE_INFINITY).toBeLessThanOrEqual(
    page.viewportSize()?.width ?? 0,
  );
});
