import { expect, type Page } from "@playwright/test";

/** Wait for the Dioxus runtime, not just the dev server's build placeholder. */
export async function gotoBuiltApp(page: Page, path = "/") {
  await page.goto(path, { waitUntil: "domcontentloaded" });

  // The Dioxus dev client replaces its rebuild placeholder when the current
  // WASM bundle is ready. Repeated navigation here would restart that client
  // before it has a chance to perform the replacement.
  await expect(page.locator("main")).toBeAttached({
    timeout: 20_000,
  });
}
