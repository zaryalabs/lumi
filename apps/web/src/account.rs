//! Persistent account gate and the server-backed Stage 3 library.

use bip39::{Language, Mnemonic};
use dioxus::prelude::*;
use gloo_net::http::Request;
use lumi_core::{
    decode_auth_bytes, encode_auth_bytes, AcceptedImport, AccountSummary, AuthChallenge,
    ChallengeResponse, CompleteLoginRequest, CreateChallengeRequest, DerivedAuthMaterial, Job,
    JobStatus, LibraryEntry, LibraryState, MaterialImportStatus, RegisterAccountRequest,
    SessionBootstrap, UpdateLibraryStateCommand,
};
use uuid::Uuid;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::RequestCredentials;

pub(crate) const API_BASE: &str = match option_env!("LUMI_API_BASE") {
    Some(value) => value,
    None => "/api/v1",
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppRoute {
    Library,
    Reader(Uuid),
}

fn initial_route() -> AppRoute {
    web_sys::window()
        .and_then(|window| window.location().hash().ok())
        .and_then(|hash| hash.strip_prefix("#reader/").map(str::to_owned))
        .and_then(|id| Uuid::parse_str(&id).ok())
        .map_or(AppRoute::Library, AppRoute::Reader)
}

fn set_browser_route(route: AppRoute) {
    let hash = match route {
        AppRoute::Library => "library".to_owned(),
        AppRoute::Reader(material_id) => format!("reader/{material_id}"),
    };
    if let Some(window) = web_sys::window() {
        let _ = window.location().set_hash(&hash);
    }
}

#[derive(Clone)]
enum AccountState {
    Loading,
    SignedOut,
    SignedIn(AccountSummary),
    Failed(String),
}

#[component]
pub(crate) fn AccountGate() -> Element {
    let mut state = use_signal(|| AccountState::Loading);
    let mut route = use_signal(initial_route);
    let mut csrf = use_signal(String::new);
    use_effect(move || {
        spawn(async move {
            match load_account().await {
                Ok(account) => {
                    csrf.set(read_cookie("lumi_csrf").unwrap_or_default());
                    state.set(AccountState::SignedIn(account));
                }
                Err(ApiError::Unauthorized) => state.set(AccountState::SignedOut),
                Err(error) => state.set(AccountState::Failed(error.to_string())),
            }
        });
    });

    let account_state = state.read().clone();
    match account_state {
        AccountState::Loading => rsx! {
            main { class: "account-screen", aria_label: "Загрузка аккаунта",
                p { class: "eyebrow", "Lumi account" }
                h1 { "Проверяем сессию…" }
            }
        },
        AccountState::SignedOut => rsx! {
            AccountEntry {
                on_authenticated: move |session: SessionBootstrap| {
                    csrf.set(session.csrf_token.clone());
                    state.set(AccountState::SignedIn(session.account));
                }
            }
        },
        AccountState::SignedIn(account) => {
            let csrf_for_logout = csrf.read().clone();
            let account_label = account
                .nickname
                .as_deref()
                .unwrap_or("без псевдонима")
                .to_owned();
            rsx! {
                div { class: "library-app",
                    header { class: "library-topbar",
                        a { class: "library-brand", href: "#library", aria_label: "Lumi — библиотека", onclick: move |_| route.set(AppRoute::Library),
                            span { class: "brand-mark", aria_hidden: "true", "L" }
                            strong { "Lumi" }
                        }
                        nav { aria_label: "Основная навигация",
                            a { href: "#library", aria_current: if route() == AppRoute::Library { "page" } else { "false" }, onclick: move |_| route.set(AppRoute::Library), "Библиотека" }
                        }
                        div { class: "account-session-bar", role: "region", aria_label: "Активная сессия",
                            span { "{account_label}" }
                            button {
                                r#type: "button",
                                onclick: move |_| {
                                    let csrf_token = csrf_for_logout.clone();
                                    spawn(async move {
                                        if logout(&csrf_token).await.is_ok() {
                                            state.set(AccountState::SignedOut);
                                            csrf.set(String::new());
                                        }
                                    });
                                },
                                "Выйти"
                            }
                        }
                    }
                    if let AppRoute::Reader(material_id) = route() {
                        crate::reader::ReaderApp {
                            material_id,
                            csrf_token: csrf.read().clone(),
                            on_close: move |_| {
                                set_browser_route(AppRoute::Library);
                                route.set(AppRoute::Library);
                            }
                        }
                    } else {
                        LibraryApp {
                            csrf_token: csrf.read().clone(),
                            on_open_reader: move |material_id| {
                                let next = AppRoute::Reader(material_id);
                                set_browser_route(next);
                                route.set(next);
                            }
                        }
                    }
                }
            }
        }
        AccountState::Failed(message) => rsx! {
            main { class: "account-screen", aria_label: "Ошибка аккаунта",
                p { class: "eyebrow", "Account unavailable" }
                h1 { "Не удалось подключиться к Lumi" }
                p { class: "account-error", role: "alert", "{message}" }
                button { r#type: "button", onclick: move |_| state.set(AccountState::SignedOut), "Открыть вход" }
            }
        },
    }
}

#[component]
fn LibraryApp(csrf_token: String, on_open_reader: EventHandler<Uuid>) -> Element {
    let mut entries = use_signal(|| Option::<Vec<LibraryEntry>>::None);
    let mut error = use_signal(String::new);
    let mut upload_open = use_signal(|| false);
    let mut details = use_signal(|| Option::<LibraryEntry>::None);
    let mut delete_candidate = use_signal(|| Option::<LibraryEntry>::None);

    use_effect(move || {
        spawn(async move {
            match load_materials().await {
                Ok(loaded) => entries.set(Some(loaded)),
                Err(load_error) => error.set(load_error.to_string()),
            }
        });
    });

    let snapshot = entries.read().clone().unwrap_or_default();
    let loaded = entries.read().is_some();
    let active_entries = snapshot
        .iter()
        .filter(|entry| entry.library_state == LibraryState::Active)
        .cloned()
        .collect::<Vec<_>>();
    let archived_entries = snapshot
        .iter()
        .filter(|entry| entry.library_state == LibraryState::Archived)
        .cloned()
        .collect::<Vec<_>>();

    rsx! {
        main { id: "library", class: "library-view", aria_label: "Библиотека Lumi",
            header { class: "library-hero",
                div {
                    p { class: "eyebrow", "Личное пространство" }
                    h1 { "Ваша библиотека" }
                    p { class: "library-lead", "EPUB-материалы, сохранённые в вашей облачной реплике." }
                }
                button {
                    class: "primary-action add-material",
                    r#type: "button",
                    onclick: move |_| upload_open.set(true),
                    "＋ Добавить EPUB"
                }
            }

            if !error().is_empty() {
                div { class: "library-alert", role: "alert",
                    span { "{error}" }
                    button { r#type: "button", onclick: move |_| {
                        error.set(String::new());
                        spawn(async move {
                            match load_materials().await {
                                Ok(loaded) => entries.set(Some(loaded)),
                                Err(load_error) => error.set(load_error.to_string()),
                            }
                        });
                    }, "Повторить" }
                }
            }

            if !loaded {
                section { class: "library-loading", aria_label: "Загрузка библиотеки", aria_live: "polite",
                    span { class: "loading-mark", aria_hidden: "true" }
                    p { "Загружаем материалы…" }
                }
            } else if active_entries.is_empty() {
                section { class: "library-empty", aria_label: "Пустая библиотека",
                    div { class: "empty-glyph", aria_hidden: "true", "L" }
                    p { class: "eyebrow", "Первый материал" }
                    h2 { "Здесь пока тихо" }
                    p { "Добавьте DRM-free reflowable EPUB — Lumi сохранит исходник и покажет честное состояние импорта." }
                    button { class: "primary-action", r#type: "button", onclick: move |_| upload_open.set(true), "Выбрать EPUB" }
                }
            } else {
                section { class: "library-section", aria_label: "Активные материалы",
                    div { class: "section-heading",
                        div {
                            p { class: "eyebrow", "Все материалы" }
                            h2 { "Недавнее" }
                        }
                        span { "{active_entries.len()} в библиотеке" }
                    }
                    div { class: "material-grid", aria_live: "polite",
                        for entry in active_entries {
                            MaterialCard {
                                key: "{entry.id}",
                                entry,
                                csrf_token: csrf_token.clone(),
                                on_changed: move |_| {
                                    spawn(async move {
                                        match load_materials().await {
                                            Ok(loaded) => entries.set(Some(loaded)),
                                            Err(load_error) => error.set(load_error.to_string()),
                                        }
                                    });
                                },
                                on_details: move |entry| details.set(Some(entry)),
                                on_delete: move |entry| delete_candidate.set(Some(entry)),
                                on_open_reader,
                                on_error: move |message| error.set(message),
                            }
                        }
                    }
                }
            }

            if !archived_entries.is_empty() {
                section { class: "library-section archived-section", aria_label: "Архив",
                    div { class: "section-heading",
                        div {
                            p { class: "eyebrow", "Сохранено вне полки" }
                            h2 { "Архив" }
                        }
                        span { "{archived_entries.len()}" }
                    }
                    div { class: "material-grid",
                        for entry in archived_entries {
                            MaterialCard {
                                key: "archived-{entry.id}",
                                entry,
                                csrf_token: csrf_token.clone(),
                                on_changed: move |_| {
                                    spawn(async move {
                                        if let Ok(loaded) = load_materials().await {
                                            entries.set(Some(loaded));
                                        }
                                    });
                                },
                                on_details: move |entry| details.set(Some(entry)),
                                on_delete: move |entry| delete_candidate.set(Some(entry)),
                                on_open_reader,
                                on_error: move |message| error.set(message),
                            }
                        }
                    }
                }
            }
        }

        if upload_open() {
            UploadDialog {
                csrf_token: csrf_token.clone(),
                on_close: move |_| upload_open.set(false),
                on_accepted: move |accepted: AcceptedImport| {
                    spawn(async move {
                        if let Ok(loaded) = load_materials().await {
                            entries.set(Some(loaded));
                        }
                        let _ = wait_for_job(accepted.job).await;
                        if let Ok(loaded) = load_materials().await {
                            entries.set(Some(loaded));
                        }
                    });
                },
            }
        }

        if let Some(entry) = details.read().clone() {
            MaterialDetailsDialog { entry, on_close: move |_| details.set(None) }
        }

        if let Some(entry) = delete_candidate.read().clone() {
            dialog { class: "library-dialog confirm-dialog", open: true, aria_label: "Удаление материала",
                p { class: "eyebrow danger-text", "Необратимо в интерфейсе" }
                h2 { "Удалить «{entry.display_title()}»?" }
                p { "Материал исчезнет из библиотеки. Сервер сохранит sync tombstone для согласованности реплик." }
                div { class: "dialog-actions",
                    button { class: "secondary-action", r#type: "button", onclick: move |_| delete_candidate.set(None), "Отмена" }
                    button { class: "danger-action", r#type: "button", onclick: move |_| {
                        let csrf = csrf_token.clone();
                        let material_id = entry.id;
                        spawn(async move {
                            match delete_material(material_id, &csrf).await {
                                Ok(()) => {
                                    delete_candidate.set(None);
                                    if let Ok(loaded) = load_materials().await {
                                        entries.set(Some(loaded));
                                    }
                                }
                                Err(delete_error) => error.set(delete_error.to_string()),
                            }
                        });
                    }, "Удалить" }
                }
            }
        }
    }
}

#[component]
fn MaterialCard(
    entry: LibraryEntry,
    csrf_token: String,
    on_changed: EventHandler<()>,
    on_details: EventHandler<LibraryEntry>,
    on_delete: EventHandler<LibraryEntry>,
    on_open_reader: EventHandler<Uuid>,
    on_error: EventHandler<String>,
) -> Element {
    let status_label = material_status_label(entry.import_status);
    let status_class = material_status_class(entry.import_status);
    let title = entry.display_title().to_owned();
    let material_id = entry.id;
    let job_id = entry.latest_job.id;
    let archived = entry.library_state == LibraryState::Archived;
    let details_entry = entry.clone();
    let delete_entry = entry.clone();
    let state_csrf = csrf_token.clone();
    let cancel_job_csrf = csrf_token.clone();
    let retry_job_csrf = csrf_token;
    let state_changed = on_changed;
    let job_changed = on_changed;
    let state_error = on_error;
    let job_error = on_error;

    rsx! {
        article { class: "material-card", aria_label: "Материал {title}",
            div { class: "material-cover", "data-state": "{status_class}", aria_hidden: "true",
                span { "EPUB" }
                strong { "{cover_monogram(&title)}" }
            }
            div { class: "material-copy",
                div { class: "material-card-heading",
                    div {
                        span { class: "format-label", "EPUB · книга" }
                        h3 { "{title}" }
                    }
                    span { class: "status-pill {status_class}", "{status_label}" }
                }
                p { class: "source-name", "{entry.source_identity.source_name}" }
                if matches!(entry.import_status, MaterialImportStatus::Queued | MaterialImportStatus::Importing) {
                    div { class: "import-progress", role: "status",
                        span { class: "progress-shimmer" }
                        p { "{job_stage_label(entry.latest_job.stage)}" }
                    }
                }
                if matches!(entry.import_status, MaterialImportStatus::Failed | MaterialImportStatus::Cancelled) {
                    div { class: "card-diagnostics",
                        for diagnostic in entry.latest_job.diagnostics.iter().take(2) {
                            p { role: "status", strong { "{diagnostic.code}" } " · {diagnostic.message}" }
                        }
                    }
                }
                div { class: "material-actions",
                    if entry.import_status == MaterialImportStatus::Ready && !archived {
                        button { class: "read-action", r#type: "button", onclick: move |_| on_open_reader.call(material_id), "Читать" }
                    }
                    button { class: "text-action", r#type: "button", onclick: move |_| on_details.call(details_entry.clone()), "Сведения" }
                    a { class: "text-action", href: "{API_BASE}/materials/{material_id}/source", "Скачать исходник" }
                    if matches!(entry.latest_job.status, JobStatus::Queued | JobStatus::Running) {
                        button { class: "text-action", r#type: "button", onclick: move |_| {
                            let csrf = cancel_job_csrf.clone();
                            spawn(async move {
                                match mutate_job(job_id, "cancel", &csrf).await {
                                    Ok(job) => {
                                        let _ = wait_for_job(job).await;
                                        job_changed.call(());
                                    }
                                    Err(error) => job_error.call(error.to_string()),
                                }
                            });
                        }, "Отменить" }
                    }
                    if matches!(entry.latest_job.status, JobStatus::Failed | JobStatus::Cancelled) {
                        button { class: "text-action", r#type: "button", onclick: move |_| {
                            let csrf = retry_job_csrf.clone();
                            spawn(async move {
                                match mutate_job(job_id, "retry", &csrf).await {
                                    Ok(job) => {
                                        job_changed.call(());
                                        let _ = wait_for_job(job).await;
                                        job_changed.call(());
                                    }
                                    Err(error) => job_error.call(error.to_string()),
                                }
                            });
                        }, "Повторить" }
                    }
                    button { class: "text-action", r#type: "button", onclick: move |_| {
                        let csrf = state_csrf.clone();
                        let target = if archived { LibraryState::Active } else { LibraryState::Archived };
                        spawn(async move {
                            match change_library_state(material_id, target, &csrf).await {
                                Ok(_) => state_changed.call(()),
                                Err(error) => state_error.call(error.to_string()),
                            }
                        });
                    }, if archived { "Вернуть" } else { "В архив" } }
                    button { class: "text-action danger-text", r#type: "button", onclick: move |_| on_delete.call(delete_entry.clone()), "Удалить" }
                }
            }
        }
    }
}

#[derive(Clone)]
struct SelectedEpub {
    name: String,
    bytes: Vec<u8>,
}

#[component]
fn UploadDialog(
    csrf_token: String,
    on_close: EventHandler<()>,
    on_accepted: EventHandler<AcceptedImport>,
) -> Element {
    let mut selected = use_signal(|| Option::<SelectedEpub>::None);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);
    rsx! {
        dialog { class: "library-dialog upload-dialog", open: true, aria_label: "Добавить EPUB",
            div { class: "dialog-heading",
                div {
                    p { class: "eyebrow", "Новый материал" }
                    h2 { "Добавить EPUB" }
                }
                button { class: "icon-action", r#type: "button", aria_label: "Закрыть загрузку", disabled: busy(), onclick: move |_| on_close.call(()), "×" }
            }
            p { "DRM-free reflowable EPUB до 100 MiB. Исходник сохраняется до запуска безопасного импортера." }
            label { class: "upload-dropzone",
                span { class: "upload-icon", aria_hidden: "true", "＋" }
                strong { if let Some(upload) = selected.read().as_ref() { "{upload.name}" } else { "Выберите файл EPUB" } }
                small { if let Some(upload) = selected.read().as_ref() { "{upload.bytes.len()} байт" } else { ".epub · до 100 MiB" } }
                input {
                    r#type: "file",
                    accept: ".epub,application/epub+zip",
                    disabled: busy(),
                    aria_label: "Файл EPUB",
                    onchange: move |event| {
                        let Some(file) = event.files().into_iter().next() else { return; };
                        spawn(async move {
                            let name = file.name();
                            match file.read_bytes().await {
                                Ok(bytes) => selected.set(Some(SelectedEpub { name, bytes: bytes.to_vec() })),
                                Err(_) => error.set("Не удалось прочитать выбранный EPUB.".to_owned()),
                            }
                        });
                    },
                }
            }
            if !error().is_empty() {
                p { class: "account-error", role: "alert", "{error}" }
            }
            div { class: "dialog-actions",
                button { class: "secondary-action", r#type: "button", disabled: busy(), onclick: move |_| on_close.call(()), "Отмена" }
                button { class: "primary-action", r#type: "button", disabled: busy() || selected.read().is_none(), onclick: move |_| {
                    let Some(upload) = selected.read().clone() else { return; };
                    let csrf = csrf_token.clone();
                    busy.set(true);
                    error.set(String::new());
                    spawn(async move {
                        match upload_epub(&csrf, &upload).await {
                            Ok(accepted) => {
                                on_accepted.call(accepted);
                                on_close.call(());
                            }
                            Err(upload_error) => {
                                error.set(upload_error.to_string());
                                busy.set(false);
                            }
                        }
                    });
                }, if busy() { "Отправляем…" } else { "Добавить в библиотеку" } }
            }
        }
    }
}

#[component]
fn MaterialDetailsDialog(entry: LibraryEntry, on_close: EventHandler<()>) -> Element {
    let revision = entry
        .active_revision_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "будет создана после импорта".to_owned());
    rsx! {
        dialog { class: "library-dialog details-dialog", open: true, aria_label: "Сведения о материале",
            div { class: "dialog-heading",
                div {
                    p { class: "eyebrow", "Сведения о материале" }
                    h2 { "{entry.display_title()}" }
                }
                button { class: "icon-action", r#type: "button", aria_label: "Закрыть сведения", onclick: move |_| on_close.call(()), "×" }
            }
            dl { class: "technical-details",
                div { dt { "Источник" } dd { "{entry.source_identity.source_name}" } }
                div { dt { "Состояние" } dd { "{material_status_label(entry.import_status)}" } }
                div { dt { "Материал" } dd { "{entry.id}" } }
                div { dt { "Ревизия" } dd { "{revision}" } }
                div { dt { "SHA-256" } dd { "{entry.source_identity.source_hash}" } }
            }
            if !entry.latest_job.diagnostics.is_empty() {
                section { class: "details-diagnostics", aria_label: "Диагностика импорта",
                    h3 { "Диагностика" }
                    for diagnostic in &entry.latest_job.diagnostics {
                        p { strong { "{diagnostic.code}" } " · {diagnostic.message}" }
                    }
                }
            }
            div { class: "dialog-actions",
                a { class: "secondary-action", href: "{API_BASE}/materials/{entry.id}/source", "Скачать исходник" }
                button { class: "primary-action", r#type: "button", onclick: move |_| on_close.call(()), "Готово" }
            }
        }
    }
}

#[component]
fn AccountEntry(on_authenticated: EventHandler<SessionBootstrap>) -> Element {
    let mut tab = use_signal(|| "register".to_owned());
    let mut nickname = use_signal(String::new);
    let mut phrase = use_signal(String::new);
    let mut confirmed = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);

    rsx! {
        main { class: "account-screen", aria_label: "Lumi — регистрация и вход",
            section { class: "account-card",
                p { class: "eyebrow", "Persistent account" }
                h1 { "Lumi" }
                p { "Seed phrase остаётся в браузере. Сервер хранит только публичный ключ и отзывную сессию." }
                div { class: "account-tabs", role: "tablist", aria_label: "Действие с аккаунтом",
                    button { r#type: "button", role: "tab", aria_selected: tab() == "register", onclick: move |_| tab.set("register".to_owned()), "Создать аккаунт" }
                    button { r#type: "button", role: "tab", aria_selected: tab() == "login", onclick: move |_| tab.set("login".to_owned()), "Войти / восстановить" }
                }
                if tab() == "register" {
                    label { class: "account-field",
                        span { "Псевдоним (необязательно)" }
                        input { value: "{nickname}", maxlength: "80", autocomplete: "nickname", oninput: move |event| nickname.set(event.value()) }
                    }
                    if phrase().is_empty() {
                        button { class: "primary-action", r#type: "button", onclick: move |_| match Mnemonic::generate_in(Language::English, 24) {
                            Ok(mnemonic) => phrase.set(mnemonic.to_string()),
                            Err(generate_error) => error.set(generate_error.to_string()),
                        }, "Сгенерировать recovery phrase" }
                    } else {
                        div { class: "seed-phrase", aria_label: "Recovery phrase", code { "{phrase}" } }
                        label { class: "account-confirm",
                            input { r#type: "checkbox", checked: confirmed(), onchange: move |event| confirmed.set(event.checked()) }
                            span { "Я сохранил(а) все 24 слова. Без них доступ нельзя восстановить." }
                        }
                        button { class: "primary-action", r#type: "button", disabled: busy() || !confirmed(), onclick: move |_| {
                            let seed_phrase = phrase.read().clone();
                            let display_name = nickname.read().clone();
                            busy.set(true);
                            error.set(String::new());
                            spawn(async move {
                                match register(&seed_phrase, &display_name).await {
                                    Ok(session) => { phrase.set(String::new()); on_authenticated.call(session); }
                                    Err(register_error) => error.set(register_error.to_string()),
                                }
                                busy.set(false);
                            });
                        }, if busy() { "Создаём…" } else { "Создать аккаунт" } }
                    }
                } else {
                    label { class: "account-field",
                        span { "Recovery phrase (24 слова)" }
                        textarea { rows: "5", value: "{phrase}", autocomplete: "off", spellcheck: "false", oninput: move |event| phrase.set(event.value()) }
                    }
                    button { class: "primary-action", r#type: "button", disabled: busy() || phrase().trim().is_empty(), onclick: move |_| {
                        let seed_phrase = phrase.read().clone();
                        busy.set(true);
                        error.set(String::new());
                        spawn(async move {
                            match login(&seed_phrase).await {
                                Ok(session) => { phrase.set(String::new()); on_authenticated.call(session); }
                                Err(login_error) => error.set(login_error.to_string()),
                            }
                            busy.set(false);
                        });
                    }, if busy() { "Проверяем…" } else { "Войти" } }
                }
                if !error().is_empty() { p { class: "account-error", role: "alert", "{error}" } }
            }
        }
    }
}

#[derive(Debug)]
enum ApiError {
    Unauthorized,
    Message(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => formatter.write_str("Сессия не найдена."),
            Self::Message(message) => formatter.write_str(message),
        }
    }
}

async fn load_account() -> Result<AccountSummary, ApiError> {
    let response = Request::get(&format!("{API_BASE}/account/me"))
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(network_error)?;
    if response.status() == 401 {
        return Err(ApiError::Unauthorized);
    }
    parse_json(response).await
}

async fn load_materials() -> Result<Vec<LibraryEntry>, ApiError> {
    let response = Request::get(&format!("{API_BASE}/materials"))
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(network_error)?;
    parse_json(response).await
}

async fn register(phrase: &str, nickname: &str) -> Result<SessionBootstrap, ApiError> {
    let material = derive_material(phrase)?;
    let request = RegisterAccountRequest {
        lookup_id: encode_auth_bytes(&material.lookup_id()),
        public_key: encode_auth_bytes(material.verifying_key().as_bytes()),
        nickname: (!nickname.trim().is_empty()).then(|| nickname.trim().to_owned()),
        device_name: browser_device_name(),
        idempotency_key: Uuid::now_v7().to_string(),
    };
    post_json("/auth/register", &request).await
}

async fn login(phrase: &str) -> Result<SessionBootstrap, ApiError> {
    let material = derive_material(phrase)?;
    let challenge_response: ChallengeResponse = post_json(
        "/auth/challenges",
        &CreateChallengeRequest {
            lookup_id: encode_auth_bytes(&material.lookup_id()),
        },
    )
    .await?;
    let challenge = AuthChallenge {
        id: challenge_response.challenge_id,
        lookup_id: decode_auth_bytes(&challenge_response.lookup_id).map_err(contract_error)?,
        nonce: decode_auth_bytes(&challenge_response.nonce).map_err(contract_error)?,
        audience: challenge_response.audience,
        expires_at: challenge_response.expires_at,
    };
    if encode_auth_bytes(&challenge.signing_bytes()) != challenge_response.transcript {
        return Err(ApiError::Message(
            "Сервер вернул несовпадающий challenge transcript.".to_owned(),
        ));
    }
    let now = (js_sys::Date::now() / 1_000.0) as u64;
    let audience = browser_origin()?;
    let signature = material
        .sign_challenge(&challenge, &audience, now)
        .map_err(contract_error)?;
    post_json(
        "/auth/login",
        &CompleteLoginRequest {
            challenge_id: challenge.id,
            signature: encode_auth_bytes(&signature.to_bytes()),
            device_name: browser_device_name(),
        },
    )
    .await
}

async fn logout(csrf: &str) -> Result<(), ApiError> {
    let response = Request::post(&format!("{API_BASE}/auth/logout"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .send()
        .await
        .map_err(network_error)?;
    if response.ok() {
        Ok(())
    } else {
        Err(ApiError::Message(format!(
            "Выход завершился с HTTP {}.",
            response.status()
        )))
    }
}

async fn upload_epub(csrf: &str, upload: &SelectedEpub) -> Result<AcceptedImport, ApiError> {
    let bytes = js_sys::Uint8Array::from(upload.bytes.as_slice());
    let parts = js_sys::Array::new();
    parts.push(&bytes);
    let blob = web_sys::Blob::new_with_u8_array_sequence(&parts)
        .map_err(|_| ApiError::Message("Не удалось подготовить EPUB к отправке.".to_owned()))?;
    let form = web_sys::FormData::new()
        .map_err(|_| ApiError::Message("Browser FormData недоступен.".to_owned()))?;
    form.append_with_blob_and_filename("file", &blob, &upload.name)
        .map_err(|_| ApiError::Message("Не удалось добавить EPUB в форму.".to_owned()))?;
    let request = Request::post(&format!("{API_BASE}/imports"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .header("Idempotency-Key", &Uuid::now_v7().to_string())
        .body(form)
        .map_err(network_error)?;
    let response = request.send().await.map_err(network_error)?;
    parse_json(response).await
}

async fn change_library_state(
    material_id: Uuid,
    library_state: LibraryState,
    csrf: &str,
) -> Result<LibraryEntry, ApiError> {
    let command = UpdateLibraryStateCommand {
        material_id,
        library_state,
    };
    let request = Request::patch(&format!("{API_BASE}/materials/{material_id}/library-state"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .header("Idempotency-Key", &Uuid::now_v7().to_string())
        .json(&command)
        .map_err(network_error)?;
    parse_json(request.send().await.map_err(network_error)?).await
}

async fn delete_material(material_id: Uuid, csrf: &str) -> Result<(), ApiError> {
    let response = Request::delete(&format!("{API_BASE}/materials/{material_id}"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .header("Idempotency-Key", &Uuid::now_v7().to_string())
        .send()
        .await
        .map_err(network_error)?;
    if response.ok() {
        Ok(())
    } else {
        Err(api_response_error(&response))
    }
}

async fn wait_for_job(initial: Job) -> Result<Job, ApiError> {
    let mut job = initial;
    for _ in 0..120 {
        if matches!(
            job.status,
            JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
        ) {
            return Ok(job);
        }
        browser_delay(100).await;
        let response = Request::get(&format!("{API_BASE}/jobs/{}", job.id))
            .credentials(RequestCredentials::Include)
            .send()
            .await
            .map_err(network_error)?;
        job = parse_json(response).await?;
    }
    Err(ApiError::Message(
        "Импорт продолжается дольше ожидаемого; его состояние сохранено.".to_owned(),
    ))
}

async fn browser_delay(milliseconds: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            let _ = window
                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, milliseconds);
        } else {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
        }
    });
    let _ = JsFuture::from(promise).await;
}

async fn mutate_job(job_id: Uuid, action: &str, csrf: &str) -> Result<Job, ApiError> {
    let response = Request::post(&format!("{API_BASE}/jobs/{job_id}/{action}"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .send()
        .await
        .map_err(network_error)?;
    parse_json(response).await
}

fn material_status_label(status: MaterialImportStatus) -> &'static str {
    match status {
        MaterialImportStatus::Queued => "В очереди",
        MaterialImportStatus::Importing => "Импортируется",
        MaterialImportStatus::Ready => "Готово",
        MaterialImportStatus::Failed => "Ошибка",
        MaterialImportStatus::Cancelled => "Отменено",
    }
}

fn material_status_class(status: MaterialImportStatus) -> &'static str {
    match status {
        MaterialImportStatus::Ready => "success",
        MaterialImportStatus::Failed | MaterialImportStatus::Cancelled => "danger",
        MaterialImportStatus::Queued | MaterialImportStatus::Importing => "pending",
    }
}

fn job_stage_label(stage: lumi_core::JobStage) -> &'static str {
    match stage {
        lumi_core::JobStage::SourceAccepted => "Исходник сохранён",
        lumi_core::JobStage::ValidatingContainer => "Проверяем контейнер",
        lumi_core::JobStage::Normalizing => "Нормализуем главы",
        lumi_core::JobStage::Persisting => "Публикуем результат",
        lumi_core::JobStage::ReaderDocumentBuilt => "Готовим документ чтения",
        lumi_core::JobStage::Committed => "Импорт завершён",
    }
}

fn cover_monogram(title: &str) -> String {
    title
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(3)
        .collect::<String>()
        .to_uppercase()
}

async fn post_json<T, R>(path: &str, value: &T) -> Result<R, ApiError>
where
    T: serde::Serialize + ?Sized,
    R: for<'de> serde::Deserialize<'de>,
{
    let request = Request::post(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .json(value)
        .map_err(network_error)?;
    let response = request.send().await.map_err(network_error)?;
    parse_json(response).await
}

async fn parse_json<T>(response: gloo_net::http::Response) -> Result<T, ApiError>
where
    T: for<'de> serde::Deserialize<'de>,
{
    if !response.ok() {
        return Err(api_response_error(&response));
    }
    response.json().await.map_err(network_error)
}

fn api_response_error(response: &gloo_net::http::Response) -> ApiError {
    if response.status() == 401 {
        ApiError::Unauthorized
    } else {
        ApiError::Message(format!("Lumi API вернул HTTP {}.", response.status()))
    }
}

fn derive_material(phrase: &str) -> Result<DerivedAuthMaterial, ApiError> {
    let mnemonic =
        Mnemonic::parse_in_normalized(Language::English, phrase.trim()).map_err(|_| {
            ApiError::Message("Нужна корректная recovery phrase из 24 слов.".to_owned())
        })?;
    if mnemonic.word_count() != 24 {
        return Err(ApiError::Message(
            "Recovery phrase должна содержать ровно 24 слова.".to_owned(),
        ));
    }
    let entropy: [u8; 32] = mnemonic
        .to_entropy()
        .try_into()
        .map_err(|_| ApiError::Message("Recovery phrase должна кодировать 256 бит.".to_owned()))?;
    DerivedAuthMaterial::derive(&entropy).map_err(contract_error)
}

fn browser_origin() -> Result<String, ApiError> {
    web_sys::window()
        .ok_or_else(|| ApiError::Message("Browser window недоступен.".to_owned()))?
        .location()
        .origin()
        .map_err(|_| ApiError::Message("Browser origin недоступен.".to_owned()))
}

fn browser_device_name() -> String {
    "Lumi Web browser".to_owned()
}

fn read_cookie(name: &str) -> Option<String> {
    let document = web_sys::window()?.document()?;
    let cookies = document
        .dyn_into::<web_sys::HtmlDocument>()
        .ok()?
        .cookie()
        .ok()?;
    cookies
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then(|| value.to_owned()))
}

fn network_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::Message(format!("Сеть/API недоступны: {error}"))
}

fn contract_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::Message(format!("Auth challenge отклонён: {error}"))
}
