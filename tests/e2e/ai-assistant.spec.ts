import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";

const supportedMarkdown = readFileSync(
  new URL("../fixtures/markdown/supported.md", import.meta.url),
);

async function register(page: import("@playwright/test").Page) {
  await page.goto("/");
  await page
    .getByRole("button", { name: "Создать фразу восстановления" })
    .click();
  await page.getByText("Я сохранил(а) все 24 слова", { exact: false }).click();
  await page.getByRole("button", { name: "Создать аккаунт" }).click();
  await expect(
    page.getByRole("region", { name: "Пустая библиотека" }),
  ).toBeVisible();
}

async function selectReaderText(page: import("@playwright/test").Page) {
  const source = page
    .locator("[data-reader-source='true']")
    .filter({ hasText: /\S/ })
    .first();
  await expect(source).toBeVisible();
  await source.evaluate((element) => {
    const text = element.firstChild;
    if (!text || text.nodeType !== Node.TEXT_NODE) {
      throw new Error("reader source span has no direct text node");
    }
    const range = document.createRange();
    range.setStart(text, 0);
    range.setEnd(text, Math.min(12, text.textContent?.length ?? 0));
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
    element.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
  });
}

async function configureOpenRouter(page: import("@playwright/test").Page) {
  await page.getByRole("button", { name: "ИИ-чат" }).click();
  const chat = page.getByRole("complementary", {
    name: "Персональный AI-ассистент",
  });
  await chat.getByRole("button", { name: "Настройки OpenRouter" }).click();
  await chat.getByLabel("Ключ OpenRouter").fill("sk-e2e-openrouter");
  await chat.getByRole("button", { name: "Проверить и сохранить" }).click();
  await expect(chat.getByText("готов", { exact: true })).toBeVisible();
  await chat.getByRole("button", { name: "Настройки OpenRouter" }).click();
  await chat.getByRole("button", { name: "Свернуть AI-чат" }).click();
}

async function importMarkdown(page: import("@playwright/test").Page) {
  await page
    .getByRole("button", { name: "＋ Добавить материал", exact: true })
    .click();
  const uploadDialog = page.getByRole("dialog", {
    name: "Добавить материал",
  });
  await uploadDialog.getByRole("tab", { name: "Markdown" }).click();
  await uploadDialog.getByLabel("Файл Markdown").setInputFiles({
    name: "supported.md",
    mimeType: "text/markdown",
    buffer: supportedMarkdown,
  });
  await uploadDialog
    .getByRole("button", { name: "Добавить в библиотеку" })
    .click();
  const card = page.getByRole("article", {
    name: "Материал Руководство Lumi",
  });
  await expect(card.getByText("Готово", { exact: true })).toBeVisible();
  return card;
}

test("persists OpenRouter chat and returns a Reader citation", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await register(page);
  await configureOpenRouter(page);
  const chat = page.getByRole("complementary", {
    name: "Персональный AI-ассистент",
  });
  const card = await importMarkdown(page);
  await card.getByRole("button", { name: "Читать" }).click();

  await selectReaderText(page);
  await page.getByRole("button", { name: "Объясни проще" }).click();
  await expect(chat).toBeVisible();
  await expect(
    chat.getByText("Ответ фикстуры основан на прикреплённом источнике."),
  ).toBeVisible();
  const citation = chat.getByRole("button", { name: /^Источник 1:/ });
  await expect(citation).toBeVisible();

  await page.reload();
  await page.getByRole("button", { name: "ИИ-чат" }).click();
  await expect(
    chat.getByText("Ответ фикстуры основан на прикреплённом источнике."),
  ).toBeVisible();
  await chat.getByRole("button", { name: /^Источник 1:/ }).click();
  await expect(
    page.getByRole("main", { name: "Чтение Руководство Lumi" }),
  ).toBeVisible();
  await expect(page.getByText("Открыт источник ответа AI.")).toBeVisible();
});

test("keeps a manual summary edit and offers regeneration as a candidate", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await register(page);
  await configureOpenRouter(page);
  await importMarkdown(page);

  const card = page.getByRole("article", {
    name: "Материал Руководство Lumi",
  });
  await card
    .getByRole("button", { name: "Дополнительные действия с материалом" })
    .click();
  await card.getByRole("button", { name: "Сведения" }).click();
  const details = page.getByRole("dialog", { name: "Сведения о материале" });
  await details.getByRole("button", { name: "Саммари материала" }).click();
  const summary = page.getByRole("dialog", { name: "Саммари" });
  await summary.getByRole("button", { name: "Создать саммари" }).click();
  await expect(
    summary.getByText("Краткое саммари фикстуры с проверяемым источником."),
  ).toBeVisible();
  await expect(
    summary.getByRole("button", { name: /^Источник ctx:/ }),
  ).toBeVisible();

  await summary.getByRole("button", { name: "Редактировать" }).click();
  await summary.getByLabel("Текст саммари").fill("Моя сохранённая версия.");
  await summary.getByRole("button", { name: "Сохранить правку" }).click();
  await expect(summary.getByText("Моя сохранённая версия.")).toBeVisible();
  await expect(summary.getByText("Отредактировано вручную")).toBeVisible();

  await summary.getByRole("button", { name: "Перегенерировать" }).click();
  const candidate = summary.getByRole("region", {
    name: "Новая версия саммари",
  });
  await expect(
    candidate.getByText("Новая версия не заменила ручную правку"),
  ).toBeVisible();
  await expect(summary.getByText("Моя сохранённая версия.")).toBeVisible();

  await summary.getByRole("button", { name: "Закрыть саммари" }).click();
  await details.getByRole("button", { name: "Готово" }).click();
  await page.getByRole("link", { name: "AI-задачи" }).click();
  await page.getByLabel("Показывать завершённые").check();
  const queue = page.getByRole("main", { name: "Очередь AI-задач" });
  const completedSummaries = queue
    .getByRole("row")
    .filter({ hasText: "Саммари" })
    .filter({ hasText: "Готово" });
  await expect(completedSummaries).toHaveCount(2);
});
