const app = document.querySelector(".app-shell");
const libraryView = document.querySelector("#library");
const readerView = document.querySelector("#reader");
const tocPanel = document.querySelector("#toc-panel");
const notesPanel = document.querySelector("#notes-panel");
const settingsPanel = document.querySelector("#reader-settings");
const tocButton = document.querySelector("#toggle-toc");
const notesButton = document.querySelector("#toggle-notes");
const settingsButton = document.querySelector("#toggle-settings");
const readerPages = [...document.querySelectorAll(".reader-page")];
const previousButton = document.querySelector("#previous-page");
const nextButton = document.querySelector("#next-page");
const pageIndicator = document.querySelector("#page-indicator");
const chapterProgress = document.querySelector(".chapter-progress span");
const toast = document.querySelector("#toast");
let currentPage = 0;
let toastTimeout;

function showToast(message) {
  window.clearTimeout(toastTimeout);
  toast.textContent = message;
  toast.classList.add("visible");
  toastTimeout = window.setTimeout(
    () => toast.classList.remove("visible"),
    2400,
  );
}

function showView(view) {
  const isReader = view === "reader";
  libraryView.hidden = isReader;
  readerView.hidden = !isReader;
  app.dataset.view = view;
  document
    .querySelector("#library-navigation")
    .toggleAttribute("aria-current", !isReader);
  window.scrollTo({ top: 0, behavior: "instant" });
}

function setPanel(panel, button, open) {
  panel.hidden = !open;
  button.setAttribute("aria-expanded", String(open));
}

function togglePanel(panel, button, otherPanel, otherButton) {
  const shouldOpen = panel.hidden;
  if (window.matchMedia("(max-width: 1120px)").matches && shouldOpen) {
    setPanel(otherPanel, otherButton, false);
  }
  setPanel(panel, button, shouldOpen);
}

function selectPage(index) {
  currentPage = Math.max(0, Math.min(index, readerPages.length - 1));
  readerPages.forEach((page, pageIndex) => {
    page.hidden = pageIndex !== currentPage;
  });
  previousButton.disabled = currentPage === 0;
  nextButton.disabled = currentPage === readerPages.length - 1;
  pageIndicator.textContent =
    currentPage === 0 ? "34% · 1 из 2" : "52% · 2 из 2";
  chapterProgress.style.width = currentPage === 0 ? "34%" : "52%";
  document.querySelector(".reading-column").scrollIntoView({ block: "start" });
}

document.querySelectorAll(".open-reader").forEach((button) => {
  button.addEventListener("click", () => showView("reader"));
});

document.querySelector("#back-to-library").addEventListener("click", () => {
  setPanel(tocPanel, tocButton, false);
  setPanel(notesPanel, notesButton, false);
  settingsPanel.hidden = true;
  settingsButton.setAttribute("aria-expanded", "false");
  showView("library");
});

tocButton.addEventListener("click", () =>
  togglePanel(tocPanel, tocButton, notesPanel, notesButton),
);

notesButton.addEventListener("click", () =>
  togglePanel(notesPanel, notesButton, tocPanel, tocButton),
);

settingsButton.addEventListener("click", () => {
  settingsPanel.hidden = !settingsPanel.hidden;
  settingsButton.setAttribute("aria-expanded", String(!settingsPanel.hidden));
});

document.querySelectorAll(".close-panel").forEach((button) => {
  button.addEventListener("click", () => {
    if (button.dataset.panel === "toc") setPanel(tocPanel, tocButton, false);
    if (button.dataset.panel === "notes")
      setPanel(notesPanel, notesButton, false);
  });
});

document.querySelectorAll("[data-page]").forEach((button) => {
  button.addEventListener("click", () => {
    selectPage(Number(button.dataset.page));
    if (window.matchMedia("(max-width: 1120px)").matches) {
      setPanel(tocPanel, tocButton, false);
    }
  });
});

previousButton.addEventListener("click", () => selectPage(currentPage - 1));
nextButton.addEventListener("click", () => selectPage(currentPage + 1));

document.querySelector("#font-size").addEventListener("input", (event) => {
  document.documentElement.style.setProperty(
    "--reader-font-size",
    `${event.target.value}%`,
  );
});

document.querySelector("#line-width").addEventListener("input", (event) => {
  document.documentElement.style.setProperty(
    "--reader-measure",
    `${event.target.value}rem`,
  );
});

document.querySelectorAll('input[name="theme"]').forEach((input) => {
  input.addEventListener("change", (event) => {
    app.dataset.theme = event.target.value;
    document.querySelector('meta[name="theme-color"]').content =
      event.target.value === "night" ? "#171a18" : "#f4f0e8";
  });
});

const selectionToolbar = document.querySelector("#selection-toolbar");
const activeHighlight = document.querySelector("#active-highlight");

function toggleSelectionToolbar() {
  selectionToolbar.hidden = !selectionToolbar.hidden;
}

activeHighlight.addEventListener("click", toggleSelectionToolbar);
activeHighlight.addEventListener("keydown", (event) => {
  if (event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    toggleSelectionToolbar();
  }
});

document.querySelector("#selection-note").addEventListener("click", () => {
  setPanel(notesPanel, notesButton, true);
  if (window.matchMedia("(max-width: 1120px)").matches) {
    setPanel(tocPanel, tocButton, false);
  }
  document.querySelector("#quick-note-input").focus();
});

document.querySelector("#selection-explain").addEventListener("click", () => {
  showToast("ИИ-действие сохранено как будущий сценарий");
});

document.querySelector("#selection-card").addEventListener("click", () => {
  showToast("Карточка сохранена как будущий сценарий");
});

document.querySelector(".margin-marker").addEventListener("click", () => {
  setPanel(notesPanel, notesButton, true);
  if (window.matchMedia("(max-width: 1120px)").matches) {
    setPanel(tocPanel, tocButton, false);
  }
});

document
  .querySelector("#quick-note-form")
  .addEventListener("submit", (event) => {
    event.preventDefault();
    const input = document.querySelector("#quick-note-input");
    if (!input.value.trim()) {
      input.focus();
      return;
    }
    input.value = "";
    showToast("Заметка сохранена локально в прототипе");
  });

const materialDialog = document.querySelector("#material-dialog");
const importDialog = document.querySelector("#import-dialog");

document.querySelector("#show-details").addEventListener("click", () => {
  materialDialog.showModal();
});

document.querySelector("#import-details").addEventListener("click", () => {
  importDialog.showModal();
});

document.querySelectorAll(".close-dialog").forEach((button) => {
  button.addEventListener("click", () => button.closest("dialog").close());
});

document.querySelector("#archive-button").addEventListener("click", (event) => {
  const archived = event.currentTarget.textContent === "Вернуть из архива";
  event.currentTarget.textContent = archived
    ? "Архивировать"
    : "Вернуть из архива";
  document.querySelector(".material-menu").removeAttribute("open");
  showToast(archived ? "Материал возвращён" : "Материал перемещён в архив");
});

document.querySelector("#download-source").addEventListener("click", () => {
  const exportData = {
    source: "attention-reading.epub",
    title: "Архитектура внимательного чтения",
    prototype: true,
  };
  const link = document.createElement("a");
  link.href = URL.createObjectURL(
    new Blob([JSON.stringify(exportData, null, 2)], {
      type: "application/json",
    }),
  );
  link.download = "lumi-prototype-material.json";
  link.click();
  URL.revokeObjectURL(link.href);
  document.querySelector(".material-menu").removeAttribute("open");
  showToast("Демонстрационный файл подготовлен");
});

document.querySelector("#import-button").addEventListener("click", () => {
  showToast("Загрузка EPUB показана как отдельный будущий сценарий");
});

document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape") return;
  selectionToolbar.hidden = true;
  settingsPanel.hidden = true;
  settingsButton.setAttribute("aria-expanded", "false");
});

selectPage(0);
