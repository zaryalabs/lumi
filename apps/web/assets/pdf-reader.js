const pdfjsModuleUrl = "/assets/pdfjs/pdf.mjs";
const pdfjsLib = await import(pdfjsModuleUrl);

pdfjsLib.GlobalWorkerOptions.workerSrc = "/assets/pdfjs/pdf.worker.mjs";

const readers = new Map();

function dispatch(container, name, detail) {
  container.dispatchEvent(
    new CustomEvent(name, {
      bubbles: true,
      detail,
    }),
  );
}

function renderAnnotations(state, shell) {
  const layer = shell.querySelector(".pdf-annotation-layer");
  layer.replaceChildren();
  const pageIndex = Number(shell.dataset.pdfPage) - 1;
  const pageWidth = Number(shell.dataset.pageWidth);
  const pageHeight = Number(shell.dataset.pageHeight);
  if (!(pageWidth > 0 && pageHeight > 0)) return;
  for (const annotation of state.annotations) {
    if (annotation.pageIndex !== pageIndex) continue;
    for (const rect of annotation.rects) {
      const marker = document.createElement("span");
      marker.className = `pdf-annotation-rect ${annotation.kind}`;
      marker.style.left = `${(rect.x / pageWidth) * 100}%`;
      marker.style.top = `${(rect.y / pageHeight) * 100}%`;
      marker.style.width = `${(rect.width / pageWidth) * 100}%`;
      marker.style.height = `${(rect.height / pageHeight) * 100}%`;
      layer.append(marker);
    }
  }
}

function pageScale(container, page, zoom) {
  const available = Math.max(container.clientWidth - 48, 280);
  return (available / page.view[2]) * zoom;
}

async function renderTextLayer(page, viewport, layer) {
  const content = await page.getTextContent();
  const measure = document.createElement("canvas").getContext("2d");
  for (const item of content.items) {
    if (!("str" in item) || item.str.length === 0) continue;
    const style = content.styles[item.fontName] ?? {};
    const transform = pdfjsLib.Util.transform(
      viewport.transform,
      item.transform,
    );
    const angle = Math.atan2(transform[1], transform[0]);
    const fontHeight = Math.hypot(transform[2], transform[3]);
    const ascent = style.ascent
      ? style.ascent * fontHeight
      : style.descent
        ? (1 + style.descent) * fontHeight
        : fontHeight;
    const span = document.createElement("span");
    span.textContent = item.str;
    span.dataset.pdfText = "true";
    span.style.left = `${transform[4]}px`;
    span.style.top = `${transform[5] - ascent}px`;
    span.style.fontSize = `${fontHeight}px`;
    span.style.fontFamily = style.fontFamily || "sans-serif";
    const transforms = [];
    if (angle !== 0) transforms.push(`rotate(${angle}rad)`);
    if (item.width > 0 && measure) {
      measure.font = `${fontHeight}px ${span.style.fontFamily}`;
      const measured = measure.measureText(item.str).width;
      if (measured > 0) {
        transforms.push(`scaleX(${(item.width * viewport.scale) / measured})`);
      }
    }
    span.style.transform = transforms.join(" ");
    layer.append(span);
  }
}

function safeExternalUrl(value) {
  try {
    const parsed = new URL(value);
    return ["http:", "https:"].includes(parsed.protocol) ? parsed.href : null;
  } catch {
    return null;
  }
}

async function renderLinks(page, viewport, layer, state) {
  const annotations = await page.getAnnotations({ intent: "display" });
  for (const annotation of annotations) {
    if (annotation.subtype !== "Link" || !annotation.rect) continue;
    const [left, bottom, right, top] = annotation.rect;
    const corners = [
      viewport.convertToViewportPoint(left, bottom),
      viewport.convertToViewportPoint(left, top),
      viewport.convertToViewportPoint(right, bottom),
      viewport.convertToViewportPoint(right, top),
    ];
    const xCoordinates = corners.map(([x]) => x);
    const yCoordinates = corners.map(([, y]) => y);
    const x1 = Math.min(...xCoordinates);
    const y1 = Math.min(...yCoordinates);
    const x2 = Math.max(...xCoordinates);
    const y2 = Math.max(...yCoordinates);
    const link = document.createElement("a");
    link.className = "pdf-link-hit-area";
    link.style.left = `${Math.min(x1, x2)}px`;
    link.style.top = `${Math.min(y1, y2)}px`;
    link.style.width = `${Math.abs(x2 - x1)}px`;
    link.style.height = `${Math.abs(y2 - y1)}px`;
    const externalUrl = annotation.url
      ? safeExternalUrl(annotation.url)
      : null;
    if (externalUrl) {
      link.href = externalUrl;
      link.target = "_blank";
      link.rel = "noopener noreferrer";
      link.ariaLabel = "Открыть внешнюю ссылку";
    } else if (annotation.dest) {
      link.href = "#";
      link.ariaLabel = "Перейти по внутренней ссылке";
      link.addEventListener("click", async (event) => {
        event.preventDefault();
        const destination =
          typeof annotation.dest === "string"
            ? await state.document.getDestination(annotation.dest)
            : annotation.dest;
        const reference = destination?.[0];
        if (!reference) return;
        const targetPage =
          typeof reference === "number"
            ? reference
            : await state.document.getPageIndex(reference);
        goToPage(state.container.id, targetPage);
      });
    } else {
      continue;
    }
    layer.append(link);
  }
}

async function renderPage(state, pageNumber) {
  if (state.rendered.has(pageNumber) || state.destroyed) return;
  const shell = state.container.querySelector(
    `[data-pdf-page="${pageNumber}"]`,
  );
  if (!shell) return;
  state.rendered.add(pageNumber);
  shell.dataset.renderState = "loading";
  try {
    const page = await state.document.getPage(pageNumber);
    const scale = pageScale(state.container, page, state.zoom);
    const viewport = page.getViewport({ scale });
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    shell.style.width = `${viewport.width}px`;
    shell.style.height = `${viewport.height}px`;
    const canvas = shell.querySelector("canvas");
    canvas.width = Math.floor(viewport.width * dpr);
    canvas.height = Math.floor(viewport.height * dpr);
    canvas.style.width = `${viewport.width}px`;
    canvas.style.height = `${viewport.height}px`;
    const context = canvas.getContext("2d", { alpha: false });
    const transform = dpr === 1 ? null : [dpr, 0, 0, dpr, 0, 0];
    await page.render({
      canvasContext: context,
      viewport,
      transform,
    }).promise;
    const textLayer = shell.querySelector(".pdf-text-layer");
    const linkLayer = shell.querySelector(".pdf-link-layer");
    textLayer.replaceChildren();
    linkLayer.replaceChildren();
    textLayer.style.width = `${viewport.width}px`;
    textLayer.style.height = `${viewport.height}px`;
    linkLayer.style.width = `${viewport.width}px`;
    linkLayer.style.height = `${viewport.height}px`;
    await Promise.all([
      renderTextLayer(page, viewport, textLayer),
      renderLinks(page, viewport, linkLayer, state),
    ]);
    shell.dataset.pageWidth = String(page.view[2]);
    shell.dataset.pageHeight = String(page.view[3]);
    renderAnnotations(state, shell);
    shell.dataset.renderState = "ready";
  } catch (error) {
    state.rendered.delete(pageNumber);
    shell.dataset.renderState = "failed";
    console.error("Lumi PDF page rendering failed", error);
    dispatch(state.container, "lumi-pdf-error", {
      message: String(error),
      pageNumber,
    });
  }
}

function clearRenderedPages(state) {
  state.rendered.clear();
  for (const shell of state.container.querySelectorAll("[data-pdf-page]")) {
    shell.dataset.renderState = "idle";
    const canvas = shell.querySelector("canvas");
    canvas.width = 0;
    canvas.height = 0;
    shell.querySelector(".pdf-text-layer").replaceChildren();
    shell.querySelector(".pdf-link-layer").replaceChildren();
    shell.querySelector(".pdf-annotation-layer").replaceChildren();
  }
}

function selectionDetail(state) {
  const selection = window.getSelection();
  if (!selection || selection.isCollapsed || selection.rangeCount === 0) {
    return null;
  }
  const range = selection.getRangeAt(0);
  const ancestor =
    range.commonAncestorContainer.nodeType === Node.ELEMENT_NODE
      ? range.commonAncestorContainer
      : range.commonAncestorContainer.parentElement;
  const shell = ancestor?.closest?.("[data-pdf-page]");
  if (!shell || !state.container.contains(shell)) return null;
  const shellRect = shell.getBoundingClientRect();
  const pageWidth = Number(shell.dataset.pageWidth);
  const pageHeight = Number(shell.dataset.pageHeight);
  if (!(pageWidth > 0 && pageHeight > 0)) return null;
  const scaleX = pageWidth / shellRect.width;
  const scaleY = pageHeight / shellRect.height;
  const rects = Array.from(range.getClientRects())
    .filter((rect) => rect.width > 0 && rect.height > 0)
    .slice(0, 256)
    .map((rect) => ({
      x: (rect.left - shellRect.left) * scaleX,
      y: (rect.top - shellRect.top) * scaleY,
      width: rect.width * scaleX,
      height: rect.height * scaleY,
    }));
  if (rects.length === 0) return null;
  return {
    pageIndex: Number(shell.dataset.pdfPage) - 1,
    quote: selection.toString().trim().slice(0, 65536),
    rects,
  };
}

async function mount(config) {
  const container = document.getElementById(config.containerId);
  if (!container) throw new Error("PDF reader container is missing");
  await destroy(config.containerId);
  container.replaceChildren();
  container.dataset.pdfState = "loading";
  const loadingTask = pdfjsLib.getDocument({
    url: config.sourceUrl,
    withCredentials: true,
    cMapUrl: "/assets/pdfjs/cmaps/",
    cMapPacked: true,
    standardFontDataUrl: "/assets/pdfjs/standard_fonts/",
    rangeChunkSize: 65536,
  });
  const documentProxy = await loadingTask.promise;
  const state = {
    container,
    document: documentProxy,
    loadingTask,
    rendered: new Set(),
    zoom: 1,
    observer: null,
    destroyed: false,
    annotations: config.annotations || [],
  };
  readers.set(config.containerId, state);
  const fragment = document.createDocumentFragment();
  for (let pageNumber = 1; pageNumber <= documentProxy.numPages; pageNumber++) {
    const model = config.pages[pageNumber - 1];
    const shell = document.createElement("section");
    shell.className = "pdf-page-shell";
    shell.dataset.pdfPage = String(pageNumber);
    shell.dataset.renderState = "idle";
    shell.ariaLabel = `Страница ${model?.page_label || pageNumber}`;
    shell.style.aspectRatio = `${model?.width_points || 612} / ${model?.height_points || 792}`;
    const canvas = document.createElement("canvas");
    canvas.className = "pdf-page-canvas";
    const textLayer = document.createElement("div");
    textLayer.className = "pdf-text-layer";
    const linkLayer = document.createElement("div");
    linkLayer.className = "pdf-link-layer";
    const annotationLayer = document.createElement("div");
    annotationLayer.className = "pdf-annotation-layer";
    shell.append(canvas, annotationLayer, textLayer, linkLayer);
    fragment.append(shell);
  }
  container.append(fragment);
  state.observer = new IntersectionObserver(
    (entries) => {
      const visible = entries
        .filter((entry) => entry.isIntersecting)
        .sort((left, right) => right.intersectionRatio - left.intersectionRatio);
      for (const entry of visible) {
        renderPage(state, Number(entry.target.dataset.pdfPage));
      }
      if (visible[0]) {
        const pageNumber = Number(visible[0].target.dataset.pdfPage);
        dispatch(container, "lumi-pdf-page", {
          pageIndex: pageNumber - 1,
          pageCount: documentProxy.numPages,
        });
      }
    },
    { root: container, rootMargin: "120% 0px", threshold: [0.1, 0.5, 0.9] },
  );
  for (const shell of container.querySelectorAll("[data-pdf-page]")) {
    state.observer.observe(shell);
  }
  container.addEventListener("mouseup", () => {
    const detail = selectionDetail(state);
    if (detail) dispatch(container, "lumi-pdf-selection", detail);
  });
  container.dataset.pdfState = "ready";
  const initialPage = Math.min(
    Math.max(Number(config.initialPage || 0) + 1, 1),
    documentProxy.numPages,
  );
  container
    .querySelector(`[data-pdf-page="${initialPage}"]`)
    ?.scrollIntoView({ block: "start" });
  dispatch(container, "lumi-pdf-ready", {
    pageCount: documentProxy.numPages,
  });
}

async function destroy(containerId) {
  const state = readers.get(containerId);
  if (!state) return;
  readers.delete(containerId);
  state.destroyed = true;
  state.observer?.disconnect();
  await state.loadingTask?.destroy();
}

function setZoom(containerId, zoom) {
  const state = readers.get(containerId);
  if (!state) return;
  state.zoom = Math.min(Math.max(Number(zoom), 0.5), 3);
  clearRenderedPages(state);
  for (const shell of state.container.querySelectorAll("[data-pdf-page]")) {
    state.observer.unobserve(shell);
    state.observer.observe(shell);
  }
}

function goToPage(containerId, pageIndex) {
  const state = readers.get(containerId);
  state?.container
    .querySelector(`[data-pdf-page="${Number(pageIndex) + 1}"]`)
    ?.scrollIntoView({ behavior: "smooth", block: "start" });
}

function setAnnotations(containerId, annotations) {
  const state = readers.get(containerId);
  if (!state) return;
  state.annotations = annotations || [];
  for (const shell of state.container.querySelectorAll("[data-pdf-page]")) {
    if (shell.dataset.renderState === "ready") renderAnnotations(state, shell);
  }
}

window.LumiPdfReader = {
  mount,
  mountJson: (config) => mount(JSON.parse(config)),
  destroy,
  setZoom,
  goToPage,
  setAnnotationsJson: (containerId, annotations) =>
    setAnnotations(containerId, JSON.parse(annotations)),
};
