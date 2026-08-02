import { expect, test } from "@playwright/test";

test("exposes the v3 shell hierarchy without mixing account destinations", async ({
  page,
}) => {
  await page.goto("/");

  const primary = page.getByRole("navigation", { name: "Основная навигация" });
  await expect(primary.getByRole("button")).toHaveCount(4);
  await primary.getByRole("button", { name: "Desk" }).click();
  await expect(
    page.getByRole("heading", { name: "Desk", exact: true }),
  ).toBeVisible();

  await page.getByRole("button", { name: "Поиск", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Поиск" })).toBeVisible();
  await page.getByRole("button", { name: "Открыть профиль" }).click();
  await expect(page.getByRole("heading", { name: "Настройки" })).toBeVisible();
});

test("uses a native responsive shell instead of a website header", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/");

  const shell = page.locator(".app-shell");
  const header = page.locator(".app-header");
  await expect(header).toHaveCSS("position", "fixed");
  await expect(shell).toHaveCSS("padding-left", "232px");

  await page.setViewportSize({ width: 390, height: 844 });
  const mobileNavigation = page.getByRole("navigation", {
    name: "Мобильная навигация",
  });
  await expect(mobileNavigation.getByRole("button")).toHaveCount(4);
  await expect(mobileNavigation.getByRole("button").nth(0)).toHaveText(
    "Библиотека",
  );
  await expect(mobileNavigation.getByRole("button").nth(1)).toHaveText("Desk");
  await expect(mobileNavigation.getByRole("button").nth(2)).toHaveText(
    "Пространства",
  );
  await expect(mobileNavigation.getByRole("button").nth(3)).toHaveText(
    "Повторение",
  );
  await expect(header).toHaveCSS("position", "sticky");

  await mobileNavigation.getByRole("button", { name: "Desk" }).click();
  await expect(page.locator(".context-title")).toHaveText("Desk");
});

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

  await page.getByText("Ещё", { exact: true }).click();
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
    page.getByRole("navigation", { name: "Мобильная навигация" }),
  ).toBeVisible();

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
