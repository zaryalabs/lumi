let installPrompt = null;
let registration = null;
let reloadForUpdate = false;

const byId = (id) => document.getElementById(id);

function setOfflineState() {
  const banner = byId("offline-banner");
  if (banner) banner.hidden = navigator.onLine;
  document.documentElement.toggleAttribute("data-offline", !navigator.onLine);
}

function showUpdate() {
  const banner = byId("pwa-update-banner");
  if (banner) banner.hidden = false;
}

function updateIsSafe() {
  return (
    !document.querySelector('[data-update-safe="false"]') &&
    document.documentElement.dataset.dirty !== "true"
  );
}

function bindUi() {
  const install = byId("pwa-install-button");
  if (install && !install.dataset.bound) {
    install.dataset.bound = "true";
    install.hidden = !installPrompt;
    install.addEventListener("click", async () => {
      if (!installPrompt) return;
      installPrompt.prompt();
      await installPrompt.userChoice;
      installPrompt = null;
      install.hidden = true;
    });
  }

  const update = byId("pwa-update-button");
  if (update && !update.dataset.bound) {
    update.dataset.bound = "true";
    update.addEventListener("click", () => {
      if (!updateIsSafe()) {
        const label = byId("pwa-update-banner")?.querySelector("span");
        if (label) {
          label.textContent =
            "Обновление готово. Завершите чтение или сохраните введённый текст.";
        }
        return;
      }
      reloadForUpdate = true;
      registration?.waiting?.postMessage({ type: "SKIP_WAITING" });
    });
  }

  const dismiss = byId("pwa-update-dismiss");
  if (dismiss && !dismiss.dataset.bound) {
    dismiss.dataset.bound = "true";
    dismiss.addEventListener("click", () => {
      const banner = byId("pwa-update-banner");
      if (banner) banner.hidden = true;
    });
  }
  setOfflineState();
}

window.addEventListener("beforeinstallprompt", (event) => {
  event.preventDefault();
  installPrompt = event;
  bindUi();
});
window.addEventListener("online", setOfflineState);
window.addEventListener("offline", setOfflineState);
window.addEventListener("lumi:pwa-clear-account", () => {
  navigator.serviceWorker.controller?.postMessage({
    type: "CLEAR_ACCOUNT_STATE",
  });
});
document.addEventListener(
  "input",
  (event) => {
    if (event.target instanceof HTMLInputElement && event.target.type === "search") {
      return;
    }
    document.documentElement.dataset.dirty = "true";
  },
  true,
);
document.addEventListener(
  "submit",
  () => delete document.documentElement.dataset.dirty,
  true,
);

new MutationObserver(bindUi).observe(document.documentElement, {
  childList: true,
  subtree: true,
});
bindUi();

if ("serviceWorker" in navigator) {
  window.addEventListener("load", async () => {
    try {
      registration = await navigator.serviceWorker.register("/service-worker.js", {
        scope: "/",
        updateViaCache: "none",
      });
      if (registration.waiting) showUpdate();
      registration.addEventListener("updatefound", () => {
        const worker = registration.installing;
        worker?.addEventListener("statechange", () => {
          if (worker.state === "installed" && navigator.serviceWorker.controller) {
            showUpdate();
          }
        });
      });
      navigator.serviceWorker.addEventListener("controllerchange", () => {
        if (reloadForUpdate) window.location.reload();
      });
    } catch {
      document.documentElement.dataset.pwaUnavailable = "true";
    }
  });
}

if (window.matchMedia("(display-mode: standalone)").matches) {
  document.documentElement.dataset.displayMode = "standalone";
}
