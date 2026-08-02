import { expect, test, type Page } from "@playwright/test";
import { gotoBuiltApp } from "./app-ready.js";

async function waitForStatic(
  page: Page,
  path: string,
  ready: (contentType: string, body: string) => boolean,
) {
  let body = "";
  await expect
    .poll(
      async () => {
        const response = await page.request.get(path);
        body = await response.text();
        return (
          response.ok() && ready(response.headers()["content-type"] ?? "", body)
        );
      },
      {
        timeout: 15_000,
        message: `${path} should be served as a public asset`,
      },
    )
    .toBe(true);
  return body;
}

test("publishes an installable static-only PWA contract", async ({ page }) => {
  const manifest = JSON.parse(
    await waitForStatic(
      page,
      "/manifest.webmanifest",
      (contentType, body) =>
        contentType.includes("application/manifest+json") &&
        body.trimStart().startsWith("{"),
    ),
  );
  expect(manifest).toMatchObject({
    id: "/",
    start_url: "/#library",
    scope: "/",
    display: "standalone",
    lang: "ru",
  });
  expect(manifest.icons).toEqual(
    expect.arrayContaining([
      expect.objectContaining({ sizes: "192x192" }),
      expect.objectContaining({ sizes: "512x512" }),
      expect.objectContaining({ purpose: expect.stringContaining("maskable") }),
    ]),
  );

  for (const path of [
    "/icons/icon-192.png",
    "/icons/icon-512.png",
    "/icons/icon-maskable-512.png",
    "/icons/apple-touch-icon.png",
  ]) {
    await waitForStatic(page, path, (contentType) =>
      contentType.startsWith("image/png"),
    );
  }
  await waitForStatic(page, "/offline.html", (contentType, body) =>
    Boolean(contentType.includes("text/html") && body.includes("Lumi")),
  );

  const worker = await waitForStatic(
    page,
    "/service-worker.js",
    (contentType, body) =>
      contentType.includes("javascript") &&
      body.includes("CLEAR_ACCOUNT_STATE"),
  );
  expect(worker).toContain('pathname.startsWith("/api/v1")');
  expect(worker).toContain('pathname.startsWith("/auth")');
  expect(worker).toContain('pathname.includes("/source")');
  expect(worker).toContain('pathname.includes("/audio")');
  expect(worker).toContain("CLEAR_ACCOUNT_STATE");

  const lifecycle = await waitForStatic(
    page,
    "/pwa.js",
    (contentType, body) =>
      contentType.includes("javascript") && body.includes("SKIP_WAITING"),
  );
  expect(lifecycle).toContain('[data-update-safe="false"]');
  expect(lifecycle).toContain("SKIP_WAITING");
  expect(lifecycle.indexOf('addEventListener("click"')).toBeLessThan(
    lifecycle.indexOf("SKIP_WAITING"),
  );
});

test("keeps shell controls, focus and semantic tokens usable at the project viewport", async ({
  page,
}) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await gotoBuiltApp(page);
  await expect(
    page.getByRole("main", { name: "Lumi — регистрация и вход" }),
  ).toBeVisible();

  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
  const tokens = await page.evaluate(() => {
    const style = getComputedStyle(document.documentElement);
    return [
      "--canvas",
      "--surface-1",
      "--text-primary",
      "--border-subtle",
      "--accent-primary",
      "--focus-ring",
      "--layer-modal",
    ].map((name) => style.getPropertyValue(name).trim());
  });
  expect(tokens.every(Boolean)).toBe(true);

  const primary = page.getByRole("button", {
    name: "Создать фразу восстановления",
  });
  await primary.focus();
  await expect(primary).toBeFocused();
  const size = await primary.evaluate((element) => {
    const rect = element.getBoundingClientRect();
    return { width: rect.width, height: rect.height };
  });
  expect(size.height).toBeGreaterThanOrEqual(44);
  expect(errors).toEqual([]);
});

test("registers the root worker and leaves private requests out of Cache Storage", async ({
  page,
  context,
  browserName,
}) => {
  test.setTimeout(60_000);
  test.skip(
    browserName === "webkit",
    "Cache Storage inspection is Chromium-only",
  );
  await waitForStatic(
    page,
    "/service-worker.js",
    (contentType, body) =>
      contentType.includes("javascript") &&
      body.includes("CLEAR_ACCOUNT_STATE"),
  );
  await page.goto("/");
  const scope = await page.evaluate(async () => {
    await navigator.serviceWorker.register("/service-worker.js", {
      scope: "/",
      updateViaCache: "none",
    });
    const registration = await navigator.serviceWorker.ready;
    return registration.scope;
  });
  await page.reload();
  expect(new URL(scope).pathname).toBe("/");

  const cachedRequests = await page.evaluate(async () => {
    const names = await caches.keys();
    const requests = await Promise.all(
      names.map(async (name) => (await caches.open(name)).keys()),
    );
    return requests.flat().map((request) => request.url);
  });
  expect(
    cachedRequests.some((url) => /\/api\/v1|\/auth|\/source|\/audio/.test(url)),
  ).toBe(false);

  await context.setOffline(true);
  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(page.locator("body")).toContainText("Lumi");
  await context.setOffline(false);
});
