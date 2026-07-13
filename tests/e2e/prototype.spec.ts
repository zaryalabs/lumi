import { expect, test } from "@playwright/test";

test("moves from the library into a focused reader workspace", async ({
  page,
}) => {
  await page.goto("/");

  await expect(
    page.getByRole("main", { name: "Lumi — прототип осмысленного чтения" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Ваша библиотека" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Архитектура внимательного чтения" }),
  ).toBeVisible();

  await page.getByRole("button", { name: "Продолжить чтение" }).click();
  await expect(
    page.getByRole("region", { name: "Экран чтения" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Внимание как выбор" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Ваша библиотека" }),
  ).toBeHidden();

  await page.getByRole("button", { name: "Заметки" }).click();
  await expect(
    page.getByRole("complementary", { name: "Заметки" }),
  ).toBeVisible();
  await expect(page.getByText("Личное · приватно")).toBeVisible();

  await page.getByRole("button", { name: "Следующая страница" }).click();
  await expect(
    page.getByRole("heading", { name: "Следы прочитанного" }),
  ).toBeVisible();
  await expect(page.getByText("52% · 2 из 2")).toBeVisible();
});

test("opens contextual reading tools and applies the night theme", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Продолжить чтение" }).click();

  await page
    .getByText(
      "Возвращение к тексту — не поражение внимания, а его основная работа.",
    )
    .click();
  await expect(
    page.getByRole("toolbar", { name: "Действия с выделением" }),
  ).toBeVisible();

  await page.getByRole("button", { name: "Настройки чтения" }).click();
  await expect(
    page.getByRole("group", { name: "Настройки чтения" }),
  ).toBeVisible();
  await page.getByRole("radio", { name: "Ночь" }).check();
  await expect(page.locator(".app-shell")).toHaveAttribute(
    "data-theme",
    "night",
  );
});

test("keeps the mobile reader content-first and uses a notes sheet", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");

  await expect(
    page.getByRole("heading", { name: "Ваша библиотека" }),
  ).toBeInViewport();
  await page.getByRole("button", { name: "Продолжить чтение" }).click();
  await expect(
    page.getByRole("heading", { name: "Внимание как выбор" }),
  ).toBeVisible();

  await page.getByRole("button", { name: "Заметки" }).click();
  const notes = page.getByRole("complementary", { name: "Заметки" });
  await expect(notes).toBeVisible();
  await expect(notes).toHaveCSS("position", "fixed");
});
