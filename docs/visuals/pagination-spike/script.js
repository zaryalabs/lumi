const sourceNodes = [
  {
    id: "chapter-1/heading-1",
    kind: "heading",
    text: "Внимание как измеряемое пространство",
  },
  {
    id: "chapter-1/paragraph-1",
    kind: "paragraph",
    text: "Постраничное чтение начинается не с номера страницы, а с устойчивой позиции в тексте. Unicode-проверка 👩🏽‍💻 и ё не должна разрезать grapheme sequence. Браузер знает реальные метрики шрифта, ширину строки и высоту каждого фрагмента, поэтому Lumi просит platform adapter измерить раскладку и сохраняет только границы исходных узлов. ".repeat(
      4,
    ),
  },
  {
    id: "chapter-1/paragraph-2",
    kind: "paragraph",
    text: "Если абзац пересекает нижнюю границу, адаптер ищет последний помещающийся символьный диапазон. Следующая страница продолжает тот же node id с точного offset, так что смена темы или размера текста меняет PageMap, но не сам anchor. ".repeat(
      5,
    ),
  },
  {
    id: "chapter-1/figure-1",
    kind: "figure",
    caption: "Изображение остаётся атомарным блоком и переносится целиком.",
  },
  {
    id: "chapter-1/paragraph-3",
    kind: "paragraph",
    text: "После изображения поток продолжается без специальных EPUB-страниц. Resource уже принадлежит normalized package, а его поздняя загрузка инвалидирует layout key и запускает повторное измерение. ".repeat(
      3,
    ),
  },
  {
    id: "chapter-1/table-1",
    kind: "table",
    rows: [
      ["Граница", "Source of truth", "Поведение"],
      ["Текст", "node id + [start, end)", "binary search по Range"],
      ["Изображение", "resource ref", "перенос или масштабирование"],
      ["Таблица", "typed table node", "atomic + horizontal scroll"],
      ["Сноска", "target anchor", "reader-native переход"],
    ],
  },
  {
    id: "chapter-1/paragraph-4",
    kind: "paragraph",
    text: "Таблица проверяет самый неудобный reflowable block: она не должна быть незаметно обрезана, а горизонтальное переполнение остаётся внутри контролируемого reader surface. ".repeat(
      4,
    ),
  },
  {
    id: "chapter-1/plugin-1",
    kind: "plugin",
    label: "Typed plugin placeholder с measurement hint",
  },
  {
    id: "chapter-1/footnote-1",
    kind: "footnote",
    text: "Сноска: page number — производное значение конкретной раскладки. Для восстановления позиции Lumi использует source-backed locator, quote context и offsets, а не фразу «страница 7». ".repeat(
      3,
    ),
  },
  {
    id: "chapter-1/paragraph-5",
    kind: "paragraph",
    text: "Инкрементальный paginator может продолжать работу небольшими batches. В активном DOM остаются measurement window и соседние страницы, поэтому длинная книга не превращается в один огромный документ браузера. ".repeat(
      7,
    ),
  },
];

const textKinds = new Set(["heading", "paragraph", "footnote"]);
const graphemeSegmenter = new Intl.Segmenter("ru", { granularity: "grapheme" });
const measurement = document.querySelector("#measurement");
const visiblePage = document.querySelector("#visible-page");
const pageMapOutput = document.querySelector("#page-map");
const pageCountOutput = document.querySelector("#page-count");
const coverageStatus = document.querySelector("#coverage-status");
const pageCounter = document.querySelector("#page-counter");
const previousButton = document.querySelector("#previous");
const nextButton = document.querySelector("#next");
const fontSize = document.querySelector("#font-size");
const fontSizeValue = document.querySelector("#font-size-value");
const pageWidth = document.querySelector("#page-width");
const reflowButton = document.querySelector("#reflow");

let pages = [];
let currentPage = 0;
let generation = 0;

function createPage() {
  const page = document.createElement("article");
  page.className = "page";
  return page;
}

function textScalars(text) {
  return Array.from(text);
}

function scalarLength(text) {
  return textScalars(text).length;
}

function scalarSlice(text, start, end) {
  return textScalars(text).slice(start, end).join("");
}

function graphemeBoundaries(text) {
  const boundaries = new Set([0]);
  for (const segment of graphemeSegmenter.segment(text)) {
    boundaries.add(scalarLength(text.slice(0, segment.index)));
  }
  boundaries.add(scalarLength(text));
  return boundaries;
}

function previousGraphemeBoundary(text, start, candidate) {
  let accepted = start;
  for (const boundary of graphemeBoundaries(text)) {
    if (boundary > candidate) {
      break;
    }
    if (boundary > accepted) {
      accepted = boundary;
    }
  }
  return accepted;
}

function nextGraphemeBoundary(text, start) {
  for (const boundary of graphemeBoundaries(text)) {
    if (boundary > start) {
      return boundary;
    }
  }
  return scalarLength(text);
}

function createTextFragment(node, start, end) {
  const element = document.createElement(
    node.kind === "heading" ? "h2" : node.kind === "footnote" ? "aside" : "p",
  );
  element.dataset.nodeId = node.id;
  element.dataset.start = String(start);
  element.dataset.end = String(end);
  if (start > 0) {
    element.classList.add("continued");
  }
  element.textContent = scalarSlice(node.text, start, end);
  return element;
}

function createAtomicFragment(node) {
  if (node.kind === "figure") {
    const figure = document.createElement("figure");
    figure.dataset.nodeId = node.id;
    const image = document.createElement("div");
    image.className = "image-surface";
    image.setAttribute("role", "img");
    image.setAttribute("aria-label", "Fixture image for pagination measurement");
    image.textContent = "normalized resource";
    const caption = document.createElement("figcaption");
    caption.textContent = node.caption;
    figure.append(image, caption);
    return figure;
  }

  if (node.kind === "table") {
    const wrapper = document.createElement("div");
    wrapper.className = "table-block";
    wrapper.dataset.nodeId = node.id;
    const table = document.createElement("table");
    const [headings, ...rows] = node.rows;
    const thead = document.createElement("thead");
    const headingRow = document.createElement("tr");
    for (const value of headings) {
      const cell = document.createElement("th");
      cell.textContent = value;
      headingRow.append(cell);
    }
    thead.append(headingRow);
    const tbody = document.createElement("tbody");
    for (const values of rows) {
      const row = document.createElement("tr");
      for (const value of values) {
        const cell = document.createElement("td");
        cell.textContent = value;
        row.append(cell);
      }
      tbody.append(row);
    }
    table.append(thead, tbody);
    wrapper.append(table);
    return wrapper;
  }

  const plugin = document.createElement("section");
  plugin.className = "plugin-block";
  plugin.dataset.nodeId = node.id;
  plugin.setAttribute("aria-label", "Plugin block placeholder");
  plugin.textContent = node.label;
  return plugin;
}

function fits(page) {
  return page.scrollHeight <= page.clientHeight + 1;
}

function normalizedBreak(text, start, candidate) {
  const scalars = textScalars(text);
  if (candidate >= scalars.length) {
    return scalars.length;
  }
  const slice = scalars.slice(start, candidate);
  const boundary = Math.max(
    slice.lastIndexOf(" "),
    slice.lastIndexOf("\n"),
    slice.lastIndexOf("—"),
  );
  const wordBoundary = boundary > 24 ? start + boundary + 1 : candidate;
  return previousGraphemeBoundary(text, start, wordBoundary);
}

function maximumTextEnd(page, node, start) {
  let low = start + 1;
  let high = scalarLength(node.text);
  let best = start;

  while (low <= high) {
    const middle = Math.floor((low + high) / 2);
    const probe = createTextFragment(node, start, middle);
    page.append(probe);
    const accepted = fits(page);
    probe.remove();
    if (accepted) {
      best = middle;
      low = middle + 1;
    } else {
      high = middle - 1;
    }
  }

  return normalizedBreak(node.text, start, best);
}

function paginate(nodes) {
  measurement.replaceChildren();
  const result = [];
  let page = createPage();
  let fragments = [];
  measurement.append(page);

  const finishPage = () => {
    if (fragments.length > 0) {
      result.push({ index: result.length, fragments });
    }
    page.remove();
    page = createPage();
    fragments = [];
    measurement.append(page);
  };

  for (const node of nodes) {
    if (!textKinds.has(node.kind)) {
      let element = createAtomicFragment(node);
      page.append(element);
      if (!fits(page) && fragments.length > 0) {
        element.remove();
        finishPage();
        element = createAtomicFragment(node);
        page.append(element);
      }
      fragments.push({ id: node.id, kind: node.kind, start: 0, end: 1 });
      continue;
    }

    const textLength = scalarLength(node.text);
    let start = 0;
    while (start < textLength) {
      const whole = createTextFragment(node, start, textLength);
      page.append(whole);
      if (fits(page)) {
        fragments.push({
          id: node.id,
          kind: node.kind,
          start,
          end: textLength,
        });
        start = textLength;
        continue;
      }
      whole.remove();

      let end = maximumTextEnd(page, node, start);
      if (end <= start && fragments.length > 0) {
        finishPage();
        end = maximumTextEnd(page, node, start);
      }
      if (end <= start) {
        end = nextGraphemeBoundary(node.text, start);
      }

      const part = createTextFragment(node, start, end);
      page.append(part);
      fragments.push({ id: node.id, kind: node.kind, start, end });
      start = end;
      if (start < textLength) {
        finishPage();
      }
    }
  }

  finishPage();
  measurement.replaceChildren();
  return result;
}

function validateCoverage(nodes, pageMap) {
  const fragments = pageMap.flatMap((page) => page.fragments);
  return nodes.every((node) => {
    const matching = fragments.filter((fragment) => fragment.id === node.id);
    if (!textKinds.has(node.kind)) {
      return matching.length === 1 && matching[0].start === 0 && matching[0].end === 1;
    }
    if (matching.length === 0 || matching[0].start !== 0) {
      return false;
    }
    for (let index = 1; index < matching.length; index += 1) {
      if (matching[index - 1].end !== matching[index].start) {
        return false;
      }
    }
    const boundaries = graphemeBoundaries(node.text);
    const fragmentsUseGraphemeBoundaries = matching.every(
      (fragment) => boundaries.has(fragment.start) && boundaries.has(fragment.end),
    );
    return fragmentsUseGraphemeBoundaries && matching.at(-1).end === scalarLength(node.text);
  });
}

function renderFragment(fragment) {
  const node = sourceNodes.find((item) => item.id === fragment.id);
  return textKinds.has(node.kind)
    ? createTextFragment(node, fragment.start, fragment.end)
    : createAtomicFragment(node);
}

function renderCurrentPage() {
  const page = pages[currentPage];
  visiblePage.replaceChildren(
    ...page.fragments.map((fragment) => renderFragment(fragment)),
  );
  pageCounter.textContent = `${currentPage + 1} / ${pages.length}`;
  previousButton.disabled = currentPage === 0;
  nextButton.disabled = currentPage === pages.length - 1;
}

function layoutKey() {
  return [
    "fixture-revision-v1",
    `width:${pageWidth.value}`,
    `height:720`,
    `font:georgia-${fontSize.value}`,
    `line-height:1.62`,
    `resources:ready`,
  ].join("|");
}

async function repaginate() {
  document.body.dataset.ready = "false";
  await document.fonts.ready;
  document.documentElement.style.setProperty("--page-width", `${pageWidth.value}px`);
  document.documentElement.style.setProperty(
    "--reader-font-size",
    `${fontSize.value}px`,
  );
  fontSizeValue.textContent = `${fontSize.value} px`;

  pages = paginate(sourceNodes);
  currentPage = Math.min(currentPage, pages.length - 1);
  const coverageValid = validateCoverage(sourceNodes, pages);
  generation += 1;

  pageCountOutput.textContent = String(pages.length);
  coverageStatus.textContent = coverageValid
    ? "PageMap валиден"
    : "PageMap содержит разрыв";
  coverageStatus.className = coverageValid ? "valid" : "invalid";
  pageMapOutput.textContent = JSON.stringify(
    { layoutKey: layoutKey(), pages },
    null,
    2,
  );
  renderCurrentPage();

  window.paginationSpike = {
    coverageValid,
    generation,
    layoutKey: layoutKey(),
    nodes: sourceNodes,
    pages,
    repaginate,
  };
  document.body.dataset.ready = "true";
}

previousButton.addEventListener("click", () => {
  currentPage = Math.max(0, currentPage - 1);
  renderCurrentPage();
});

nextButton.addEventListener("click", () => {
  currentPage = Math.min(pages.length - 1, currentPage + 1);
  renderCurrentPage();
});

fontSize.addEventListener("input", repaginate);
pageWidth.addEventListener("change", repaginate);
reflowButton.addEventListener("click", repaginate);
repaginate();
