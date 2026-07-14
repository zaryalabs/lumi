import { devices, expect, test } from "@playwright/test";

const supportedEpub = Buffer.from(
  "UEsDBBQAAAAAAAAAIQBvYassFAAAABQAAAAIAAAAbWltZXR5cGVhcHBsaWNhdGlvbi9lcHViK3ppcFBLAwQUAAAACAD9eO1cHBxuKlQAAABrAAAAFgAAAE1FVEEtSU5GL2NvbnRhaW5lci54bWyzsa/IzVEoSy0qzszPs1Uy1DNQsrezSc7PK0nMzEstsrMpys8vScvMSS1GMBXSSnNydAsSSzJslVwDQp30CxKTsxPTU/XyC9KU9O1s9JH06COMAgBQSwMEFAAAAAgA/XjtXFlgKDfMAAAAbQEAABAAAABFUFVCL3BhY2thZ2Uub3BmjdA9bsMwDAXgqwhag0RxstIKEMBbhy49ACEzCVFJFiQmdW9f2c7f2E16JD48EA5j8OpGufAQW91stvpgIaH7xjO98n3NLQQS7FHQgrB4ssc8/BTKqvv8OoJZMnCZUIZsP66BVbfrwDwS8BjP1+paimCeHzAvN2DkExWxwEJBcd/qiDetLplO83MzXiR4rQL1jGv5TdRqTMmzQ6lNzTxejdNKykOiLExlQcwb6pqHKTSKcc3/XTMVftYsiSMtcOWqPaOVn9buQ3M/p/0DUEsDBBQAAAAIAP147Vxvj8P2PgAAAEgAAAAOAAAARVBVQi9uYXYueGh0bWyzySjJzbGzScpPqbSzyUsss7NJVMgoSk2zVSpJrSjRTzbUqwCpULJzzkgsKEktstFPtLPRByvUh2jSB5sAAFBLAwQUAAAACAD9eO1cT/i+nUcAAABNAAAAEgAAAEVQVUIvdGV4dC9jMS54aHRtbLPJKMnNsbNJyk+ptLPJMLRzzkgsKEktstEHsm0K7AJSi4ozi0tS80oUilITcxRcA0KdFDJzC/KLSvRs9AvsbPQhOvXBxgAAUEsBAhQDFAAAAAAAAAAhAG9hqywUAAAAFAAAAAgAAAAAAAAAAAAAAIABAAAAAG1pbWV0eXBlUEsBAhQDFAAAAAgA/XjtXBwcbipUAAAAawAAABYAAAAAAAAAAAAAAIABOgAAAE1FVEEtSU5GL2NvbnRhaW5lci54bWxQSwECFAMUAAAACAD9eO1cWWAoN8wAAABtAQAAEAAAAAAAAAAAAAAAgAHCAAAARVBVQi9wYWNrYWdlLm9wZlBLAQIUAxQAAAAIAP147Vxvj8P2PgAAAEgAAAAOAAAAAAAAAAAAAACAAbwBAABFUFVCL25hdi54aHRtbFBLAQIUAxQAAAAIAP147VxP+L6dRwAAAE0AAAASAAAAAAAAAAAAAACAASYCAABFUFVCL3RleHQvYzEueGh0bWxQSwUGAAAAAAUABQA0AQAAnQIAAAAA",
  "base64",
);

const readerEpub = Buffer.from(
  "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",
  "base64",
);

function crc32(bytes: Buffer): number {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) {
      crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function storedZip(files: Array<[string, string]>): Buffer {
  const localRecords: Buffer[] = [];
  const centralRecords: Buffer[] = [];
  let offset = 0;
  for (const [path, content] of files) {
    const name = Buffer.from(path);
    const data = Buffer.from(content);
    const checksum = crc32(data);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt32LE(checksum, 14);
    local.writeUInt32LE(data.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(name.length, 26);
    localRecords.push(local, name, data);

    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE(20, 4);
    central.writeUInt16LE(20, 6);
    central.writeUInt32LE(checksum, 16);
    central.writeUInt32LE(data.length, 20);
    central.writeUInt32LE(data.length, 24);
    central.writeUInt16LE(name.length, 28);
    central.writeUInt32LE(offset, 42);
    centralRecords.push(central, name);
    offset += local.length + name.length + data.length;
  }
  const centralDirectory = Buffer.concat(centralRecords);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(files.length, 8);
  end.writeUInt16LE(files.length, 10);
  end.writeUInt32LE(centralDirectory.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...localRecords, centralDirectory, end]);
}

function createReaderEpub(): Buffer {
  const firstChapter = "Первая глава проверяет постраничное чтение. ".repeat(
    160,
  );
  const secondChapter =
    "Вторая глава завершает книгу и сохраняет позицию. ".repeat(150);
  return storedZip([
    ["mimetype", "application/epub+zip"],
    [
      "META-INF/container.xml",
      '<?xml version="1.0"?><container><rootfiles><rootfile full-path="EPUB/package.opf" media-type="application/oebps-package+xml"/></rootfiles></container>',
    ],
    [
      "EPUB/package.opf",
      '<?xml version="1.0"?><package version="3.0"><metadata><title>Stage Four Reader</title><creator>Lumi QA</creator><language>ru</language></metadata><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="c1" href="text/c1.xhtml" media-type="application/xhtml+xml"/><item id="c2" href="text/c2.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c1"/><itemref idref="c2"/></spine></package>',
    ],
    [
      "EPUB/nav.xhtml",
      '<html><body><nav><ol><li><a href="text/c1.xhtml">Первая глава</a></li><li><a href="text/c2.xhtml">Вторая глава</a></li></ol></nav></body></html>',
    ],
    [
      "EPUB/text/c1.xhtml",
      `<html><body><h1>Первая глава</h1><p>${firstChapter}</p><p>Откройте <a href="#note-1">примечание</a> или <a href="c2.xhtml">вторую главу</a>.</p><aside id="note-1">Сноска из нормализованного документа.</aside></body></html>`,
    ],
    [
      "EPUB/text/c2.xhtml",
      `<html><body><h1>Вторая глава</h1><p>${secondChapter}</p><p>Конец книги.</p></body></html>`,
    ],
  ]);
}

async function selectReaderText(page: import("@playwright/test").Page) {
  const source = page
    .locator("[data-reader-source='true']")
    .filter({
      hasText: /\S/,
    })
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

test("retries a failed account bootstrap before offering sign-in", async ({
  page,
}) => {
  let attempts = 0;
  await page.route("**/api/v1/account/me", async (route) => {
    attempts += 1;
    if (attempts === 1) {
      await route.fulfill({ status: 503, body: "temporary bootstrap outage" });
    } else {
      await route.continue();
    }
  });
  await page.goto("/");
  await expect(
    page.getByRole("main", { name: "Ошибка аккаунта" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Повторить подключение" }).click();
  await expect.poll(() => attempts).toBeGreaterThan(1);
  await expect(
    page.getByRole("main", { name: "Lumi — регистрация и вход" }),
  ).toBeVisible();
});

test("persists an API-backed EPUB library lifecycle", async ({ page }) => {
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
    page.getByRole("region", { name: "Пустая библиотека" }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "＋ Добавить материал", exact: true })
    .click();
  let uploadDialog = page.getByRole("dialog", { name: "Добавить материал" });
  await expect(uploadDialog).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(
    page.getByRole("button", { name: "＋ Добавить материал", exact: true }),
  ).toBeFocused();
  await page
    .getByRole("button", { name: "＋ Добавить материал", exact: true })
    .click();
  uploadDialog = page.getByRole("dialog", { name: "Добавить материал" });
  const epubTab = uploadDialog.getByRole("tab", { name: "EPUB" });
  await epubTab.focus();
  await page.keyboard.press("ArrowRight");
  await expect(
    uploadDialog.getByRole("tab", { name: "Web-ссылка" }),
  ).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("ArrowLeft");
  await expect(epubTab).toHaveAttribute("aria-selected", "true");
  await uploadDialog.getByLabel("Файл EPUB").setInputFiles({
    name: "browser.epub",
    mimeType: "application/epub+zip",
    buffer: createReaderEpub(),
  });
  await expect(
    uploadDialog.getByText("browser.epub", { exact: true }),
  ).toBeVisible();
  await uploadDialog
    .getByRole("button", { name: "Добавить в библиотеку" })
    .click();
  const supportedCard = page.getByRole("article", {
    name: "Материал Stage Four Reader",
  });
  await expect(
    supportedCard.getByText("Готово", { exact: true }),
  ).toBeVisible();

  await page
    .getByRole("button", { name: "＋ Добавить материал", exact: true })
    .click();
  const webDialog = page.getByRole("dialog", { name: "Добавить материал" });
  await webDialog.getByRole("tab", { name: "Web-ссылка" }).click();
  await webDialog
    .getByLabel("URL статьи")
    .fill("https://fixtures.lumi.test/article");
  await webDialog
    .getByRole("button", { name: "Добавить в библиотеку" })
    .click();
  const webCard = page.getByRole("article", {
    name: "Материал Фикстура web-статьи",
  });
  await expect(
    webCard.getByText("Web · статья", { exact: true }),
  ).toBeVisible();
  await expect(webCard.getByText("Готово", { exact: true })).toBeVisible();
  await webCard.getByRole("button", { name: "Читать" }).click();
  const webReader = page.getByRole("main", {
    name: "Чтение Фикстура web-статьи",
  });
  await expect(
    webReader.getByText(/Первый абзац подтверждает импорт/),
  ).toBeVisible();
  const external = webReader.getByRole("link", {
    name: "Открыть: безопасный источник",
  });
  await expect(external).toHaveAttribute("target", "_blank");
  await expect(external).toHaveAttribute("rel", /noopener/);
  await page.getByRole("button", { name: "Библиотека", exact: false }).click();

  await supportedCard.getByRole("button", { name: "Читать" }).click();
  const reader = page.getByRole("main", { name: "Чтение Stage Four Reader" });
  await expect(reader).toBeVisible();
  await expect(
    page.getByRole("article", { name: /Страница 1 из/ }),
  ).toBeVisible();

  await selectReaderText(page);
  const highlightSaved = page.waitForResponse(
    (response) =>
      response.url().endsWith("/annotations") &&
      response.request().method() === "POST" &&
      response.ok(),
  );
  await page.getByRole("button", { name: "Выделить" }).click();
  await highlightSaved;
  await expect(page.locator(".annotation-highlight").first()).toBeVisible();

  await selectReaderText(page);
  await page.getByRole("button", { name: "Заметка" }).click();
  await page.getByLabel("Текст заметки").fill("Заметка Stage 5");
  const noteSaved = page.waitForResponse(
    (response) =>
      response.url().endsWith("/annotations") &&
      response.request().method() === "POST" &&
      response.ok(),
  );
  await page.getByRole("button", { name: "Сохранить заметку" }).click();
  await noteSaved;
  await page.getByRole("button", { name: /Заметки \(2\)/ }).click();
  const notes = page.getByRole("complementary", {
    name: "Личные заметки и выделения",
  });
  await expect(notes.getByText("Заметка Stage 5")).toBeVisible();
  await notes.getByRole("button", { name: "Изменить" }).click();
  await notes
    .getByLabel("Редактировать заметку")
    .fill("Заметка Stage 5 · edit");
  const noteUpdated = page.waitForResponse(
    (response) =>
      response.url().includes("/annotations/") &&
      response.request().method() === "PUT" &&
      response.ok(),
  );
  await notes.getByRole("button", { name: "Сохранить изменения" }).click();
  await noteUpdated;
  await expect(notes.getByText("Заметка Stage 5 · edit")).toBeVisible();
  page.once("dialog", (dialog) => dialog.accept());
  const highlightDeleted = page.waitForResponse(
    (response) =>
      response.url().includes("/annotations/") &&
      response.request().method() === "DELETE" &&
      response.ok(),
  );
  await notes
    .locator("li")
    .filter({ hasText: "Highlight" })
    .getByRole("button", { name: "Удалить" })
    .click();
  await highlightDeleted;
  await expect(
    page.getByRole("button", { name: /Заметки \(1\)/ }),
  ).toBeVisible();
  const annotationExport = page.waitForEvent("download");
  await page.getByRole("button", { name: "Экспорт" }).click();
  expect((await annotationExport).suggestedFilename()).toMatch(
    /^lumi-annotations-.*\.json$/,
  );
  await notes.getByRole("button", { name: "Закрыть заметки" }).click();

  await page.getByRole("button", { name: "Дальше" }).click();
  await expect(
    page.getByRole("article", { name: /Страница 2 из/ }),
  ).toBeVisible();

  await page.getByRole("button", { name: "Оглавление" }).click();
  const toc = page.getByRole("navigation", { name: "Оглавление материала" });
  await expect(toc.getByRole("button", { name: "Вторая глава" })).toBeVisible();
  await toc.getByRole("button", { name: "Вторая глава" }).click();
  await expect(
    page.getByRole("button", { name: "Назад по истории" }),
  ).toBeEnabled();
  await page.getByRole("button", { name: "Назад по истории" }).click();

  const footnoteLink = page.getByRole("button", {
    name: "Перейти: примечание",
  });
  for (let pageIndex = 0; pageIndex < 24; pageIndex += 1) {
    if (await footnoteLink.count()) break;
    const next = page.getByRole("button", { name: "Дальше" });
    if (await next.isDisabled()) break;
    await next.click();
  }
  await footnoteLink.click();
  const footnote = page.getByRole("dialog", { name: "Сноска" });
  await expect(footnote).toBeFocused();
  await expect(footnote).toContainText("Сноска из нормализованного документа");
  await page.keyboard.press("Escape");
  await expect(footnote).toHaveCount(0);
  await expect(page.getByRole("article", { name: /Страница/ })).toBeFocused();
  await page.getByRole("button", { name: "Перейти: вторую главу" }).click();
  await expect(
    page.getByRole("button", { name: "Назад по истории" }),
  ).toBeEnabled();

  await page.getByRole("button", { name: "Настройки" }).click();
  const settings = page.getByRole("complementary", {
    name: "Настройки чтения",
  });
  await settings.getByLabel("Размер текста").fill("24");
  const nightSaved = page.waitForResponse(
    (response) =>
      response.url().endsWith("/api/v1/reader/settings") &&
      response.request().method() === "PUT" &&
      (response.request().postData() ?? "").includes('"night"') &&
      response.ok(),
  );
  await settings.getByText("Ночь", { exact: true }).click();
  await expect(reader).toHaveClass(/night/);
  await nightSaved;

  await page.reload();
  await expect(
    page.getByRole("main", { name: "Чтение Stage Four Reader" }),
  ).toHaveClass(/night/);
  await expect(
    page.getByRole("article", { name: /Страница (?:[2-9]|[1-9][0-9]+) из/ }),
  ).toBeVisible();
  await page.getByRole("button", { name: /Заметки \(1\)/ }).click();
  await expect(page.getByText("Заметка Stage 5 · edit")).toBeVisible();
  await page.getByRole("button", { name: "Закрыть заметки" }).click();
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByRole("article", { name: /Страница/ })).toBeVisible();
  await page.getByRole("button", { name: "Библиотека", exact: false }).click();
  await expect(
    page.getByRole("region", { name: "Продолжить чтение" }),
  ).toContainText("Stage Four Reader");

  await page
    .getByRole("button", { name: "＋ Добавить материал", exact: true })
    .click();
  uploadDialog = page.getByRole("dialog", { name: "Добавить материал" });
  await uploadDialog.getByLabel("Файл EPUB").setInputFiles({
    name: "broken.epub",
    mimeType: "application/epub+zip",
    buffer: Buffer.from("not a ZIP container"),
  });
  await uploadDialog
    .getByRole("button", { name: "Добавить в библиотеку" })
    .click();
  const failedCard = page.getByRole("article", {
    name: "Материал broken",
  });
  await expect(failedCard.getByText("Ошибка", { exact: true })).toBeVisible();
  await expect(
    failedCard.getByText("epub_invalid_zip", { exact: false }),
  ).toBeVisible();

  await supportedCard.getByRole("button", { name: "Сведения" }).click();
  const detailsDialog = page.getByRole("dialog", {
    name: "Сведения о материале",
  });
  await expect(detailsDialog.getByText("browser.epub")).toBeVisible();
  await expect(
    detailsDialog.getByText("будет создана после импорта"),
  ).toHaveCount(0);
  await detailsDialog.getByRole("button", { name: "Готово" }).click();

  const download = page.waitForEvent("download");
  await supportedCard.getByRole("link", { name: "Скачать исходник" }).click();
  expect((await download).suggestedFilename()).toBe("browser.epub");

  await supportedCard.getByRole("button", { name: "В архив" }).click();
  const archivedCard = page
    .getByRole("region", { name: "Архив" })
    .getByRole("article", { name: "Материал Stage Four Reader" });
  await expect(archivedCard).toBeVisible();
  await expect
    .poll(async () => {
      const continuation = page.getByRole("region", {
        name: "Продолжить чтение",
      });
      return (await continuation.count()) > 0
        ? ((await continuation.textContent()) ?? "")
        : "";
    })
    .not.toContain("Stage Four Reader");
  await archivedCard.getByRole("button", { name: "Вернуть" }).click();
  await expect(supportedCard).toBeVisible();
  await expect
    .poll(async () => {
      const continuation = page.getByRole("region", {
        name: "Продолжить чтение",
      });
      return (await continuation.count()) > 0
        ? ((await continuation.textContent()) ?? "")
        : "";
    })
    .toContain("Stage Four Reader");

  await failedCard.getByRole("button", { name: "Удалить" }).click();
  const deleteDialog = page.getByRole("dialog", {
    name: "Удаление материала",
  });
  await deleteDialog.getByRole("button", { name: "Удалить" }).click();
  await expect(failedCard).toHaveCount(0);

  await page.reload();
  await expect(
    page
      .getByRole("region", { name: "Активные материалы" })
      .getByRole("article", { name: "Материал Stage Four Reader" })
      .getByText("Готово", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("navigation", { name: "Основная навигация" }),
  ).toBeHidden();

  await page.setViewportSize({ width: 1280, height: 900 });
  await expect(
    page.getByRole("navigation", { name: "Основная навигация" }),
  ).toBeVisible();

  await page.setViewportSize({ width: 390, height: 844 });
  await expect(
    page.getByRole("article", { name: "Материал Stage Four Reader" }),
  ).toBeVisible();

  await page.getByRole("button", { name: "Выйти" }).click();
  await page
    .getByRole("button", { name: "Сгенерировать recovery phrase" })
    .click();
  await page.getByText("Я сохранил(а) все 24 слова", { exact: false }).click();
  await page.getByRole("button", { name: "Создать аккаунт" }).click();
  await expect(
    page.getByRole("region", { name: "Пустая библиотека" }),
  ).toBeVisible();

  await page.getByRole("button", { name: "Выйти" }).click();
  await page.getByRole("tab", { name: "Войти / восстановить" }).click();
  await page.getByLabel("Recovery phrase (24 слова)").fill(phrase);
  await page.getByRole("button", { name: "Войти", exact: true }).click();
  await expect(
    page.getByRole("article", { name: "Материал Stage Four Reader" }),
  ).toBeVisible();

  let materialListAttempts = 0;
  await page.route("**/api/v1/materials", async (route) => {
    if (route.request().method() === "GET") {
      materialListAttempts += 1;
      if (materialListAttempts === 1) {
        await route.fulfill({ status: 503, body: "temporary library outage" });
        return;
      }
    }
    await route.continue();
  });
  await page.reload();
  await expect(
    page.getByRole("region", { name: "Библиотека временно недоступна" }),
  ).toBeVisible();
  await expect(
    page.getByRole("region", { name: "Пустая библиотека" }),
  ).toHaveCount(0);
  await page.getByRole("button", { name: "Повторить" }).click();
  await expect.poll(() => materialListAttempts).toBeGreaterThan(1);
  await expect(
    page.getByRole("article", { name: "Материал Stage Four Reader" }),
  ).toBeVisible();

  await page.context().clearCookies();
  await page
    .getByRole("article", { name: "Материал Stage Four Reader" })
    .getByRole("button", { name: "В архив" })
    .click();
  await expect(
    page.getByRole("main", { name: "Сессия истекла" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Перейти ко входу" }).click();
  await expect(
    page.getByRole("main", { name: "Lumi — регистрация и вход" }),
  ).toBeVisible();
});

test.describe("mobile touch reader", () => {
  const pixel = devices["Pixel 7"];
  test.use({
    viewport: pixel.viewport,
    userAgent: pixel.userAgent,
    deviceScaleFactor: pixel.deviceScaleFactor,
    isMobile: pixel.isMobile,
    hasTouch: pixel.hasTouch,
  });

  test("uses a safe-area bottom sheet with keyboard dismissal", async ({
    page,
  }) => {
    let capabilityAttempts = 0;
    await page.route("**/api/v1/capabilities", async (route) => {
      capabilityAttempts += 1;
      if (capabilityAttempts === 1) {
        await route.fulfill({
          status: 503,
          body: "temporary capability outage",
        });
      } else {
        await route.continue();
      }
    });
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.goto("/");
    expect(await page.evaluate(() => innerWidth)).toBeLessThanOrEqual(760);
    await expect(
      page.getByRole("main", { name: "Lumi — регистрация и вход" }),
    ).toBeVisible();
    expect(await page.evaluate(() => navigator.maxTouchPoints)).toBeGreaterThan(
      0,
    );
    expect(
      await page.evaluate(
        () => matchMedia("(prefers-reduced-motion: reduce)").matches,
      ),
    ).toBe(true);

    await page
      .getByRole("button", { name: "Сгенерировать recovery phrase" })
      .click();
    await page
      .getByText("Я сохранил(а) все 24 слова", { exact: false })
      .click();
    await page.getByRole("button", { name: "Создать аккаунт" }).click();
    const capabilityError = page.getByRole("alert").filter({
      hasText: "Не удалось проверить возможности сервера",
    });
    await expect(capabilityError).toBeVisible();
    await expect(page.getByText("Проверяем поддержку Telegram…")).toBeVisible();
    await capabilityError.getByRole("button", { name: "Повторить" }).click();
    await expect.poll(() => capabilityAttempts).toBeGreaterThan(1);
    await expect(capabilityError).toHaveCount(0);
    await page
      .getByRole("button", { name: "Добавить материал" })
      .first()
      .click();
    const dialog = page.getByRole("dialog", { name: "Добавить материал" });
    await expect(dialog.getByRole("tab", { name: "Web-ссылка" })).toBeEnabled();
    await dialog.getByLabel("Файл EPUB").setInputFiles({
      name: "mobile-reader.epub",
      mimeType: "application/epub+zip",
      buffer: createReaderEpub(),
    });
    await dialog.getByRole("button", { name: "Добавить в библиотеку" }).click();
    const card = page.getByRole("article", {
      name: "Материал Stage Four Reader",
    });
    await expect(card.getByText("Готово", { exact: true })).toBeVisible();
    await card.getByRole("button", { name: "Читать" }).click();

    const reader = page.getByRole("main", { name: "Чтение Stage Four Reader" });
    await expect(reader).toBeVisible();
    for (const name of ["Оглавление", "Настройки", "Экспорт"]) {
      await expect(reader.getByRole("button", { name })).toBeVisible();
    }

    await reader.getByRole("button", { name: "Оглавление" }).click();
    const toc = page.getByRole("navigation", { name: "Оглавление материала" });
    await expect(toc).toHaveCSS("position", "fixed");
    await expect(toc).toHaveCSS("bottom", "0px");
    await expect(
      toc.getByRole("button", { name: "Закрыть оглавление" }),
    ).toBeFocused();
    await page.keyboard.press("Shift+Tab");
    expect(
      await toc.evaluate((panel) => panel.contains(document.activeElement)),
    ).toBe(true);
    await page.keyboard.press("Escape");
    await expect(toc).toHaveCount(0);
    await expect(
      reader.getByRole("button", { name: "Оглавление" }),
    ).toBeFocused();

    await page.route("**/api/v1/reader/settings", async (route) => {
      if (route.request().method() === "PUT") {
        await new Promise((resolve) => setTimeout(resolve, 450));
      }
      await route.continue();
    });
    await reader.getByRole("button", { name: "Настройки" }).click();
    const settings = page.getByRole("complementary", {
      name: "Настройки чтения",
    });
    await expect(settings).toHaveCSS("position", "fixed");
    const settingsSaved = page.waitForResponse(
      (response) =>
        response.url().endsWith("/api/v1/reader/settings") &&
        response.request().method() === "PUT" &&
        response.ok(),
    );
    await settings.getByLabel("Размер текста").fill("24");
    await expect(reader.locator(".reader-save-state")).toContainText(
      "Сохраняем настройки…",
    );
    await expect(reader.locator(".reader-save-state")).toBeVisible();
    await settingsSaved;
    await page.keyboard.press("Escape");
    await expect(settings).toHaveCount(0);
    await expect(
      reader.getByRole("button", { name: "Настройки" }),
    ).toBeFocused();

    const annotationExport = page.waitForEvent("download");
    await reader.getByRole("button", { name: "Экспорт" }).click();
    await annotationExport;

    await reader.getByRole("button", { name: /Заметки \(0\)/ }).click();
    const notes = page.getByRole("complementary", {
      name: "Личные заметки и выделения",
    });
    await expect(
      notes.getByRole("button", { name: "Закрыть заметки" }),
    ).toBeFocused();
    await expect(notes).toHaveCSS("position", "fixed");
    await expect(notes).toHaveCSS("bottom", "0px");
    await page.keyboard.press("Escape");
    await expect(notes).toHaveCount(0);
    await expect(
      reader.getByRole("button", { name: /Заметки \(0\)/ }),
    ).toBeFocused();
  });
});
