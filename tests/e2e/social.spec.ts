import { expect, test, type BrowserContext, type Page } from "@playwright/test";

async function register(context: BrowserContext): Promise<Page> {
  const page = await context.newPage();
  await page.goto("/");
  await page
    .getByRole("button", { name: "Создать фразу восстановления" })
    .click();
  await page.getByText("Я сохранил(а) все 24 слова", { exact: false }).click();
  await page.getByRole("button", { name: "Создать аккаунт" }).click();
  await expect(
    page.getByRole("region", { name: "Пустая библиотека" }),
  ).toBeVisible();
  await expect(page.getByRole("link", { name: "Сообщества" })).toBeVisible();
  return page;
}

test("two accounts create, preview, join, revoke and remove Community access", async ({
  browser,
}) => {
  test.setTimeout(120_000);
  const ownerContext = await browser.newContext();
  const memberContext = await browser.newContext();
  const rejectedContext = await browser.newContext();
  const owner = await register(ownerContext);

  await owner.getByRole("link", { name: "Сообщества" }).click();
  const communityList = owner.getByRole("main", {
    name: "Сообщества Lumi",
  });
  await communityList.getByLabel("Название").fill("Клуб двух копий");
  await communityList
    .getByLabel("Описание")
    .fill("Без публикации исходных файлов");
  await communityList
    .getByRole("button", { name: "Создать пространство" })
    .click();

  const ownerSpace = owner.getByRole("main", {
    name: "Пространство сообщества",
  });
  await expect(
    ownerSpace.getByRole("heading", { name: "Клуб двух копий" }),
  ).toBeVisible();
  await ownerSpace
    .getByRole("button", { name: "Создать новую ссылку" })
    .click();
  const inviteInput = ownerSpace.getByLabel(
    "Скопируйте ссылку — после закрытия она больше не показывается",
  );
  await expect(inviteInput).toHaveValue(/#join\/[A-Za-z0-9_-]{43}$/);
  const inviteUrl = await inviteInput.inputValue();

  const member = await register(memberContext);
  await member.goto(inviteUrl);
  const preview = member.getByRole("main", {
    name: "Вступление в сообщество",
  });
  await expect(
    preview.getByRole("heading", { name: "Клуб двух копий" }),
  ).toBeVisible();
  await expect(
    preview.getByText("Вступление не открывает чужие файлы", {
      exact: false,
    }),
  ).toBeVisible();
  await preview.getByRole("button", { name: "Вступить" }).click();
  await expect(
    member
      .getByRole("main", { name: "Пространство сообщества" })
      .getByRole("heading", { name: "Клуб двух копий" }),
  ).toBeVisible();
  await expect(member).not.toHaveURL(/#join\//);

  await owner.reload();
  await expect(
    ownerSpace.getByText("2 участников", { exact: false }),
  ).toBeVisible();
  await ownerSpace.getByRole("button", { name: "Отозвать" }).click();
  await expect(
    ownerSpace.getByText("Отозвана", { exact: false }),
  ).toBeVisible();

  const rejected = await register(rejectedContext);
  await rejected.goto(inviteUrl);
  await expect(
    rejected.getByRole("main", { name: "Вступление в сообщество" }),
  ).toContainText("HTTP 404");

  await ownerSpace.getByRole("button", { name: "Удалить" }).click();
  await member.reload();
  await expect(
    member
      .getByRole("main", { name: "Пространство сообщества" })
      .getByRole("alert"),
  ).toContainText("HTTP 404");

  await ownerContext.close();
  await memberContext.close();
  await rejectedContext.close();
});
