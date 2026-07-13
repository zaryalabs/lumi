import { expect, test } from "@playwright/test";

const supportedEpub = Buffer.from(
  "UEsDBBQAAAAAAAAAIQBvYassFAAAABQAAAAIAAAAbWltZXR5cGVhcHBsaWNhdGlvbi9lcHViK3ppcFBLAwQUAAAACAD9eO1cHBxuKlQAAABrAAAAFgAAAE1FVEEtSU5GL2NvbnRhaW5lci54bWyzsa/IzVEoSy0qzszPs1Uy1DNQsrezSc7PK0nMzEstsrMpys8vScvMSS1GMBXSSnNydAsSSzJslVwDQp30CxKTsxPTU/XyC9KU9O1s9JH06COMAgBQSwMEFAAAAAgA/XjtXFlgKDfMAAAAbQEAABAAAABFUFVCL3BhY2thZ2Uub3BmjdA9bsMwDAXgqwhag0RxstIKEMBbhy49ACEzCVFJFiQmdW9f2c7f2E16JD48EA5j8OpGufAQW91stvpgIaH7xjO98n3NLQQS7FHQgrB4ssc8/BTKqvv8OoJZMnCZUIZsP66BVbfrwDwS8BjP1+paimCeHzAvN2DkExWxwEJBcd/qiDetLplO83MzXiR4rQL1jGv5TdRqTMmzQ6lNzTxejdNKykOiLExlQcwb6pqHKTSKcc3/XTMVftYsiSMtcOWqPaOVn9buQ3M/p/0DUEsDBBQAAAAIAP147Vxvj8P2PgAAAEgAAAAOAAAARVBVQi9uYXYueGh0bWyzySjJzbGzScpPqbSzyUsss7NJVMgoSk2zVSpJrSjRTzbUqwCpULJzzkgsKEktstFPtLPRByvUh2jSB5sAAFBLAwQUAAAACAD9eO1cT/i+nUcAAABNAAAAEgAAAEVQVUIvdGV4dC9jMS54aHRtbLPJKMnNsbNJyk+ptLPJMLRzzkgsKEktstEHsm0K7AJSi4ozi0tS80oUilITcxRcA0KdFDJzC/KLSvRs9AvsbPQhOvXBxgAAUEsBAhQDFAAAAAAAAAAhAG9hqywUAAAAFAAAAAgAAAAAAAAAAAAAAIABAAAAAG1pbWV0eXBlUEsBAhQDFAAAAAgA/XjtXBwcbipUAAAAawAAABYAAAAAAAAAAAAAAIABOgAAAE1FVEEtSU5GL2NvbnRhaW5lci54bWxQSwECFAMUAAAACAD9eO1cWWAoN8wAAABtAQAAEAAAAAAAAAAAAAAAgAHCAAAARVBVQi9wYWNrYWdlLm9wZlBLAQIUAxQAAAAIAP147Vxvj8P2PgAAAEgAAAAOAAAAAAAAAAAAAACAAbwBAABFUFVCL25hdi54aHRtbFBLAQIUAxQAAAAIAP147VxP+L6dRwAAAE0AAAASAAAAAAAAAAAAAACAASYCAABFUFVCL3RleHQvYzEueGh0bWxQSwUGAAAAAAUABQA0AQAAnQIAAAAA",
  "base64",
);

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
  const importRegion = page.getByRole("region", { name: "Импорт EPUB" });
  await importRegion.getByLabel("Файл EPUB").setInputFiles({
    name: "browser.epub",
    mimeType: "application/epub+zip",
    buffer: supportedEpub,
  });
  await expect(
    importRegion.getByText("Выбран: browser.epub", { exact: false }),
  ).toBeVisible();
  await importRegion
    .getByRole("button", { name: "Загрузить и импортировать" })
    .click();
  const supportedCard = importRegion.getByRole("article", {
    name: "Импорт Browser EPUB",
  });
  await expect(
    supportedCard.getByText("Готово", { exact: true }),
  ).toBeVisible();

  await importRegion.getByLabel("Файл EPUB").setInputFiles({
    name: "broken.epub",
    mimeType: "application/epub+zip",
    buffer: Buffer.from("not a ZIP container"),
  });
  await importRegion
    .getByRole("button", { name: "Загрузить и импортировать" })
    .click();
  const failedCard = importRegion.getByRole("article", {
    name: "Импорт broken",
  });
  await expect(failedCard.getByText("Ошибка", { exact: true })).toBeVisible();
  await expect(
    failedCard.getByText("epub_invalid_zip", { exact: false }),
  ).toBeVisible();

  await page.reload();
  await expect(
    page
      .getByRole("region", { name: "Импорт EPUB" })
      .getByRole("article", { name: "Импорт Browser EPUB" })
      .getByText("Готово", { exact: true }),
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
