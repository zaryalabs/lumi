import { expect, test } from "@playwright/test";

test("creates, restores and revokes a persistent Lumi account", async ({
  page,
}) => {
  await page.goto("/");

  await expect(
    page.getByRole("main", { name: "Lumi — регистрация и вход" }),
  ).toBeVisible();
  await expect(page.getByRole("heading", { name: "Lumi" })).toBeVisible();
  await page
    .getByRole("button", { name: "Сгенерировать recovery phrase" })
    .click();
  const phrase = (await page.getByLabel("Recovery phrase").innerText()).trim();
  expect(phrase.split(/\s+/)).toHaveLength(24);
  await page.getByText("Я сохранил(а) все 24 слова", { exact: false }).click();
  await page.getByRole("button", { name: "Создать аккаунт" }).click();

  await expect(
    page.getByRole("region", { name: "Активная сессия" }),
  ).toBeVisible();
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

  await page.getByRole("button", { name: "Выйти" }).click();
  await page.getByRole("tab", { name: "Войти / восстановить" }).click();
  await page.getByLabel("Recovery phrase (24 слова)").fill(phrase);
  await page.getByRole("button", { name: "Войти", exact: true }).click();
  await expect(
    page.getByRole("region", { name: "Активная сессия" }),
  ).toBeVisible();
});
