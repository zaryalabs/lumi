import { expect, test, type BrowserContext, type Page } from "@playwright/test";

const sharedMarkdown = Buffer.from(
  "# Клубная книга\n\n## Глава\n\nОдин и тот же текст в независимых пользовательских копиях.\n",
);
const ambiguousMarkdown = Buffer.from(
  "# Клубная книга\n\n## Другая глава\n\nСовершенно другое короткое содержание с тем же названием.\n",
);

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

async function importMarkdown(page: Page, buffer: Buffer) {
  await page.getByRole("link", { name: "Библиотека", exact: true }).click();
  await page
    .getByRole("button", { name: "＋ Добавить материал", exact: true })
    .click();
  const dialog = page.getByRole("dialog", { name: "Добавить материал" });
  await dialog.getByRole("tab", { name: "Markdown" }).click();
  await dialog.getByLabel("Файл Markdown").setInputFiles({
    name: "club-book.md",
    mimeType: "text/markdown",
    buffer,
  });
  await dialog.getByRole("button", { name: "Добавить в библиотеку" }).click();
  const card = page.getByRole("article", { name: "Материал Клубная книга" });
  await expect(card.getByText("Готово", { exact: true })).toBeVisible();
  return card;
}

async function joinByLink(page: Page, inviteUrl: string) {
  await page.goto(inviteUrl);
  await page
    .getByRole("main", { name: "Вступление в сообщество" })
    .getByRole("button", { name: "Вступить" })
    .click();
  await expect(page).not.toHaveURL(/#join\//);
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

test("shares metadata and matches only each participant's own copy", async ({
  browser,
}) => {
  test.setTimeout(120_000);
  const ownerContext = await browser.newContext();
  const memberContext = await browser.newContext();
  const reviewerContext = await browser.newContext();
  const owner = await register(ownerContext);
  const ownerCard = await importMarkdown(owner, sharedMarkdown);

  await owner.getByRole("link", { name: "Сообщества" }).click();
  const communityList = owner.getByRole("main", { name: "Сообщества Lumi" });
  await communityList.getByLabel("Название").fill("Клубная полка");
  await communityList
    .getByRole("button", { name: "Создать пространство" })
    .click();
  const ownerSpace = owner.getByRole("main", {
    name: "Пространство сообщества",
  });
  await ownerSpace
    .getByRole("button", { name: "Создать новую ссылку" })
    .click();
  const inviteInput = ownerSpace.getByLabel(
    "Скопируйте ссылку — после закрытия она больше не показывается",
  );
  const inviteUrl = await inviteInput.inputValue();

  await owner.getByRole("link", { name: "Библиотека", exact: true }).click();
  await ownerCard
    .getByRole("button", { name: "Дополнительные действия с материалом" })
    .click();
  await ownerCard
    .getByRole("button", { name: "Поделиться в сообществе" })
    .click();
  const shareDialog = owner.getByRole("dialog", {
    name: "Поделиться материалом",
  });
  await shareDialog.getByRole("button", { name: "Клубная полка" }).click();
  await expect(
    shareDialog.getByText("Исходный файл, личные заметки", { exact: false }),
  ).toBeVisible();
  await shareDialog.getByRole("button", { name: "Поделиться" }).click();

  const member = await register(memberContext);
  await importMarkdown(member, sharedMarkdown);
  await joinByLink(member, inviteUrl);
  const memberSpace = member.getByRole("main", {
    name: "Пространство сообщества",
  });
  const memberMaterial = memberSpace.getByRole("article", {
    name: "Материал сообщества Клубная книга",
  });
  await expect(
    memberMaterial.getByText("Импортируйте свою копию", { exact: true }),
  ).toBeVisible();
  await memberMaterial
    .getByRole("button", { name: "Подключить свою копию" })
    .click();
  await member
    .getByRole("dialog", { name: "Подключить свою копию" })
    .getByRole("button", { name: "Клубная книга" })
    .click();
  await expect(
    memberMaterial.getByText("Есть ваша копия", { exact: true }),
  ).toBeVisible();

  await owner.getByRole("link", { name: "Сообщества" }).click();
  await owner
    .getByRole("main", { name: "Сообщества Lumi" })
    .getByRole("article")
    .filter({ hasText: "Клубная полка" })
    .getByRole("button", { name: "Открыть" })
    .click();
  const ownerMaterial = owner
    .getByRole("main", { name: "Пространство сообщества" })
    .getByRole("article", {
      name: "Материал сообщества Клубная книга",
    });
  await ownerMaterial
    .getByRole("button", { name: "Открыть обсуждение" })
    .click();
  const ownerDiscussion = ownerMaterial.getByRole("region", {
    name: "Обсуждение материала Клубная книга",
  });
  await ownerDiscussion
    .getByLabel("Начать новое обсуждение")
    .fill("Что изменилось в вашем понимании главы?");
  await ownerDiscussion.getByRole("button", { name: "Опубликовать" }).click();
  await expect(
    ownerDiscussion.getByText("Что изменилось в вашем понимании главы?"),
  ).toBeVisible();

  await memberMaterial
    .getByRole("button", { name: "Открыть обсуждение" })
    .click();
  const memberDiscussion = memberMaterial.getByRole("region", {
    name: "Обсуждение материала Клубная книга",
  });
  await expect(
    memberDiscussion.getByText("Что изменилось в вашем понимании главы?"),
  ).toBeVisible();
  await memberDiscussion
    .getByRole("article", { name: "Комментарий участника" })
    .getByRole("button", { name: "Ответить", exact: true })
    .click();
  await memberDiscussion
    .getByLabel("Ответить на комментарий")
    .fill("Теперь лучше вижу связь между двумя тезисами.");
  await memberDiscussion
    .locator('button[type="submit"]')
    .filter({ hasText: "Ответить" })
    .click();
  await expect(
    memberDiscussion.getByText("Теперь лучше вижу связь между двумя тезисами."),
  ).toBeVisible();

  await owner.reload();
  const reloadedOwnerMaterial = owner.getByRole("article", {
    name: "Материал сообщества Клубная книга",
  });
  await reloadedOwnerMaterial
    .getByRole("button", { name: "Открыть обсуждение" })
    .click();
  const reloadedOwnerDiscussion = reloadedOwnerMaterial.getByRole("region", {
    name: "Обсуждение материала Клубная книга",
  });
  const memberReply = reloadedOwnerDiscussion
    .getByText("Теперь лучше вижу связь между двумя тезисами.")
    .locator("..");
  await memberReply
    .getByRole("button", { name: "Скрыть", exact: true })
    .click();

  await member.reload();
  const reloadedMemberMaterial = member.getByRole("article", {
    name: "Материал сообщества Клубная книга",
  });
  await reloadedMemberMaterial
    .getByRole("button", { name: "Открыть обсуждение" })
    .click();
  await expect(
    reloadedMemberMaterial.getByText("Содержимое скрыто модератором."),
  ).toBeVisible();

  const reviewer = await register(reviewerContext);
  await importMarkdown(reviewer, ambiguousMarkdown);
  await joinByLink(reviewer, inviteUrl);
  const reviewerSpace = reviewer.getByRole("main", {
    name: "Пространство сообщества",
  });
  const reviewerMaterial = reviewerSpace.getByRole("article", {
    name: "Материал сообщества Клубная книга",
  });
  await reviewerMaterial
    .getByRole("button", { name: "Подключить свою копию" })
    .click();
  await reviewer
    .getByRole("dialog", { name: "Подключить свою копию" })
    .getByRole("button", { name: "Клубная книга" })
    .click();
  await expect(
    reviewerMaterial.getByText("Нужно подтвердить", { exact: true }),
  ).toBeVisible();

  await ownerContext.close();
  await memberContext.close();
  await reviewerContext.close();
});
