//! Persistent account gate and the server-backed Stage 3 library.

use bip39::{Language, Mnemonic};
use dioxus::prelude::*;
use gloo_net::http::Request;
use lumi_core::{
    decode_auth_bytes, encode_auth_bytes, AcceptedImport, AccountSummary, AuthChallenge,
    ChallengeResponse, CompleteLoginRequest, ContinueReadingEntry, CreateChallengeRequest,
    DerivedAuthMaterial, ImportWebUrlRequest, Job, JobStatus, LibraryEntry, LibraryState,
    MaterialImportStatus, MaterialKind, ReadingProgress, RegisterAccountRequest,
    ServiceCapabilities, SessionBootstrap, TelegramBotRuntimeStatus, TelegramBotSettings,
    TelegramConnectionStatus, TelegramPairingResponse, UpdateLibraryStateCommand,
    UpdateTelegramBotTokenRequest,
};
use uuid::Uuid;
use wasm_bindgen::closure::Closure;
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
    Settings,
    Reader(Uuid),
}

fn initial_route() -> AppRoute {
    let hash = web_sys::window()
        .and_then(|window| window.location().hash().ok())
        .unwrap_or_default();
    if hash == "#settings" {
        return AppRoute::Settings;
    }
    hash.strip_prefix("#reader/")
        .and_then(|id| Uuid::parse_str(id).ok())
        .map_or(AppRoute::Library, AppRoute::Reader)
}

fn set_browser_route(route: AppRoute) {
    let hash = match route {
        AppRoute::Library => "library".to_owned(),
        AppRoute::Settings => "settings".to_owned(),
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
    Expired,
    Failed(String),
}

const SESSION_EXPIRED_EVENT: &str = "lumi:session-expired";

pub(crate) fn notify_session_expired() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(event) = web_sys::CustomEvent::new(SESSION_EXPIRED_EVENT) else {
        return;
    };
    let _ = window.dispatch_event(&event);
}

#[component]
pub(crate) fn AccountGate() -> Element {
    let mut state = use_signal(|| AccountState::Loading);
    let mut route = use_signal(initial_route);
    let mut csrf = use_signal(String::new);
    let mut bootstrap_generation = use_signal(|| 0_u64);
    use_effect(move || {
        let Some(window) = web_sys::window() else {
            return;
        };
        let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            clear_csrf_cookie();
            csrf.set(String::new());
            route.set(AppRoute::Library);
            state.set(AccountState::Expired);
        });
        let _ = window.add_event_listener_with_callback(
            SESSION_EXPIRED_EVENT,
            handler.as_ref().unchecked_ref(),
        );
        handler.forget();
    });
    use_effect(move || {
        let _ = bootstrap_generation();
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
        AccountState::Expired => rsx! {
            main { id: "main-content", class: "account-screen session-expired", aria_label: "Сессия истекла",
                section { class: "account-card", role: "alert",
                    p { class: "eyebrow danger-text", "Сессия завершена" }
                    h1 { "Войдите снова" }
                    p { "Срок сессии истёк или она была отозвана. После повторного входа Lumi восстановит уже сохранённые данные." }
                    button { class: "primary-action", r#type: "button", onclick: move |_| state.set(AccountState::SignedOut), "Перейти ко входу" }
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
                    a { class: "skip-link", href: "#main-content", "Перейти к содержанию" }
                    if !matches!(route(), AppRoute::Reader(_)) {
                    header { class: "library-topbar",
                        a { class: "library-brand", href: "#library", aria_label: "Lumi — библиотека", onclick: move |_| {
                            set_browser_route(AppRoute::Library);
                            route.set(AppRoute::Library);
                        },
                            span { class: "brand-mark", aria_hidden: "true", "L" }
                            strong { "Lumi" }
                        }
                        nav { aria_label: "Основная навигация",
                            a { href: "#library", aria_current: if route() == AppRoute::Library { "page" } else { "false" }, onclick: move |_| {
                                set_browser_route(AppRoute::Library);
                                route.set(AppRoute::Library);
                            }, "Библиотека" }
                            a { href: "#settings", aria_current: if route() == AppRoute::Settings { "page" } else { "false" }, onclick: move |_| {
                                set_browser_route(AppRoute::Settings);
                                route.set(AppRoute::Settings);
                            }, "Настройки" }
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
                    } else if route() == AppRoute::Settings {
                        SettingsApp { csrf_token: csrf.read().clone() }
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
            main { id: "main-content", class: "account-screen", aria_label: "Ошибка аккаунта",
                p { class: "eyebrow", "Account unavailable" }
                h1 { "Не удалось подключиться к Lumi" }
                p { class: "account-error", role: "alert", "{message}" }
                div { class: "dialog-actions",
                    button { class: "primary-action", r#type: "button", onclick: move |_| {
                        state.set(AccountState::Loading);
                        bootstrap_generation += 1;
                    }, "Повторить подключение" }
                    button { class: "secondary-action", r#type: "button", onclick: move |_| state.set(AccountState::SignedOut), "Открыть вход" }
                }
            }
        },
    }
}

#[component]
fn SettingsApp(csrf_token: String) -> Element {
    let mut settings = use_signal(|| Option::<TelegramBotSettings>::None);
    let mut token = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);

    use_effect(move || {
        spawn(async move {
            match load_telegram_bot_settings().await {
                Ok(value) => settings.set(Some(value)),
                Err(load_error) => error.set(load_error.to_string()),
            }
        });
    });

    let settings_snapshot = settings.read().clone();
    let configured = settings_snapshot
        .as_ref()
        .is_some_and(|value| value.configured);
    let running = settings_snapshot
        .as_ref()
        .is_some_and(|value| value.status == TelegramBotRuntimeStatus::Running);
    let bot_label = settings_snapshot
        .as_ref()
        .and_then(|value| value.bot_username.as_deref())
        .map_or_else(
            || "без username".to_owned(),
            |username| format!("@{username}"),
        );
    let bot_id_label = settings_snapshot
        .as_ref()
        .and_then(|value| value.bot_id)
        .map_or_else(|| "—".to_owned(), |id| id.to_string());
    let fingerprint_label = settings_snapshot
        .as_ref()
        .and_then(|value| value.token_fingerprint.as_deref())
        .unwrap_or("скрыт")
        .to_owned();
    let save_csrf = csrf_token.clone();
    let delete_csrf = csrf_token;

    rsx! {
        main { id: "main-content", class: "library-view settings-view", aria_label: "Настройки Lumi",
            header { class: "library-hero",
                div {
                    p { class: "eyebrow", "Конфигурация" }
                    h1 { "Настройки" }
                    p { class: "library-lead", "Подключения и параметры этого экземпляра Lumi." }
                }
            }

            section { class: "library-section telegram-settings", aria_label: "Настройки Telegram-бота",
                div { class: "section-heading",
                    div {
                        p { class: "eyebrow", "Источник" }
                        h2 { "Telegram-бот" }
                    }
                    span { class: if running { "runtime-status runtime-running" } else { "runtime-status runtime-stopped" },
                        if settings.read().is_none() && error().is_empty() {
                            "Проверяем…"
                        } else if running {
                            "Работает"
                        } else {
                            "Не работает"
                        }
                    }
                }

                p { class: "settings-notice", role: "note",
                    "Это глобальная настройка сервера. Пока в Lumi нет ролей, любой вошедший пользователь может заменить токен бота."
                }

                if let Some(current) = settings_snapshot.as_ref() {
                    if current.configured {
                        dl { class: "settings-summary",
                            div { dt { "Бот" } dd { "{bot_label}" } }
                            div { dt { "Bot ID" } dd { "{bot_id_label}" } }
                            div { dt { "Токен" } dd { "{fingerprint_label}" } }
                        }
                    }
                    if let Some(runtime_error) = current.last_error.as_ref() {
                        p { class: "account-error", role: "status", "{runtime_error}" }
                    }
                }

                if !error().is_empty() {
                    p { class: "account-error", role: "alert", "{error}" }
                }

                label { class: "account-field telegram-token-field",
                    span { if configured { "Новый токен BotFather" } else { "Токен BotFather" } }
                    input {
                        r#type: "password",
                        name: "telegram_bot_token",
                        autocomplete: "off",
                        spellcheck: "false",
                        placeholder: "123456789:AA…",
                        value: "{token}",
                        oninput: move |event| token.set(event.value()),
                    }
                }
                p { class: "capability-note", "Lumi проверит токен через Telegram, сохранит его зашифрованным и не покажет снова." }

                div { class: "material-actions",
                    button { class: "primary-action", r#type: "button", disabled: busy() || token().trim().is_empty(), onclick: move |_| {
                        let submitted_token = token.read().clone();
                        let csrf = save_csrf.clone();
                        busy.set(true);
                        error.set(String::new());
                        spawn(async move {
                            match update_telegram_bot_token(&csrf, &submitted_token).await {
                                Ok(value) => {
                                    token.set(String::new());
                                    settings.set(Some(value));
                                    for _ in 0..10 {
                                        browser_delay(500).await;
                                        match load_telegram_bot_settings().await {
                                            Ok(value) if matches!(value.status, TelegramBotRuntimeStatus::Running | TelegramBotRuntimeStatus::Degraded) => {
                                                settings.set(Some(value));
                                                break;
                                            }
                                            Ok(value) => settings.set(Some(value)),
                                            Err(load_error) => {
                                                error.set(load_error.to_string());
                                                break;
                                            }
                                        }
                                    }
                                }
                                Err(save_error) => error.set(save_error.to_string()),
                            }
                            busy.set(false);
                        });
                    }, if busy() { "Проверяем…" } else if configured { "Заменить токен" } else { "Подключить бота" } }

                    if configured {
                        button { class: "danger-action", r#type: "button", disabled: busy(), onclick: move |_| {
                            let csrf = delete_csrf.clone();
                            busy.set(true);
                            error.set(String::new());
                            spawn(async move {
                                match delete_telegram_bot_token(&csrf).await {
                                    Ok(value) => {
                                        token.set(String::new());
                                        settings.set(Some(value));
                                    }
                                    Err(delete_error) => error.set(delete_error.to_string()),
                                }
                                busy.set(false);
                            });
                        }, "Отключить бота" }
                    }
                }
            }
        }
    }
}

#[component]
fn LibraryApp(csrf_token: String, on_open_reader: EventHandler<Uuid>) -> Element {
    let entries = use_signal(|| Option::<Vec<LibraryEntry>>::None);
    let mut error = use_signal(String::new);
    let mut add_open = use_signal(|| false);
    let mut details = use_signal(|| Option::<LibraryEntry>::None);
    let mut delete_candidate = use_signal(|| Option::<LibraryEntry>::None);
    let mut telegram_status = use_signal(|| Option::<TelegramConnectionStatus>::None);
    let mut telegram_pairing = use_signal(|| Option::<TelegramPairingResponse>::None);
    let mut telegram_error = use_signal(String::new);
    let mut telegram_busy = use_signal(|| false);
    let mut capabilities = use_signal(|| Option::<ServiceCapabilities>::None);
    let continue_reading = use_signal(|| Option::<(LibraryEntry, ReadingProgress)>::None);
    let refresh_generation = use_signal(|| 0_u64);
    use_effect(move || {
        if delete_candidate.read().is_some() {
            defer_account_dialog("delete-material-dialog");
        }
    });

    use_effect(move || {
        spawn(async move {
            if let Err(load_error) =
                refresh_library(entries, continue_reading, refresh_generation).await
            {
                error.set(load_error.to_string());
            }
        });
    });
    use_effect(move || {
        spawn(async move {
            match load_capabilities().await {
                Ok(value) => capabilities.set(Some(value)),
                Err(api_error) => error.set(format!(
                    "Не удалось проверить возможности сервера: {api_error}"
                )),
            }
        });
    });
    use_effect(move || {
        let telegram_available = capabilities.read().as_ref().is_some_and(|value| {
            value
                .features
                .iter()
                .any(|feature| feature == "telegram-one-time-pairing")
        });
        if !telegram_available {
            return;
        }
        spawn(async move {
            match load_telegram_status().await {
                Ok(status) => telegram_status.set(Some(status)),
                Err(api_error) => telegram_error.set(api_error.to_string()),
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
    let telegram_unlink_csrf = csrf_token.clone();
    let telegram_pairing_csrf = csrf_token.clone();
    let capabilities_loaded = capabilities.read().is_some();
    let web_import_enabled = capabilities.read().as_ref().is_some_and(|value| {
        value
            .features
            .iter()
            .any(|feature| feature == "public-web-url-import")
    });
    let telegram_enabled = capabilities.read().as_ref().is_some_and(|value| {
        value
            .features
            .iter()
            .any(|feature| feature == "telegram-text-import")
            && value
                .features
                .iter()
                .any(|feature| feature == "telegram-one-time-pairing")
    });

    rsx! {
        main { id: "main-content", class: "library-view", aria_label: "Библиотека Lumi",
            header { class: "library-hero",
                div {
                    p { class: "eyebrow", "Личное пространство" }
                    h1 { "Ваша библиотека" }
                    p { class: "library-lead", "EPUB, web-статьи и Telegram-тексты в вашей облачной библиотеке." }
                }
                button {
                    id: "add-material-button",
                    class: "primary-action add-material",
                    r#type: "button",
                    onclick: move |_| add_open.set(true),
                    "＋ Добавить материал"
                }
            }

            if !error().is_empty() {
                div { class: "library-alert", role: "alert",
                    span { "{error}" }
                    button { r#type: "button", onclick: move |_| {
                        error.set(String::new());
                        spawn(async move {
                            if let Err(load_error) = refresh_library(entries, continue_reading, refresh_generation).await {
                                error.set(load_error.to_string());
                            }
                            match load_capabilities().await {
                                Ok(value) => capabilities.set(Some(value)),
                                Err(api_error) => error.set(format!("Не удалось проверить возможности сервера: {api_error}")),
                            }
                            match load_telegram_status().await {
                                Ok(status) => {
                                    telegram_status.set(Some(status));
                                    telegram_error.set(String::new());
                                }
                                Err(api_error) => telegram_error.set(api_error.to_string()),
                            }
                        });
                    }, "Повторить" }
                }
            }

            if !loaded && error().is_empty() {
                section { class: "library-loading", aria_label: "Загрузка библиотеки", aria_live: "polite",
                    span { class: "loading-mark", aria_hidden: "true" }
                    p { "Загружаем материалы…" }
                }
            } else if !loaded {
                section { class: "library-error-state", aria_label: "Библиотека временно недоступна",
                    h2 { "Материалы пока не показаны" }
                    p { "Используйте «Повторить» в сообщении выше. Lumi не подменяет ошибку пустой библиотекой." }
                }
            } else if active_entries.is_empty() {
                section { class: "library-empty", aria_label: "Пустая библиотека",
                    div { class: "empty-glyph", aria_hidden: "true", "L" }
                    p { class: "eyebrow", "Первый материал" }
                    h2 { "Здесь пока тихо" }
                    p { "Добавьте DRM-free EPUB или публичную web-статью — Lumi сохранит исходник и покажет честное состояние импорта." }
                    button { class: "primary-action", r#type: "button", onclick: move |_| add_open.set(true), "Добавить материал" }
                }
            } else {
                if let Some((entry, progress)) = continue_reading.read().clone() {
                    section { class: "continue-section", aria_label: "Продолжить чтение",
                        div { class: "section-heading",
                            div { p { class: "eyebrow", "Продолжить" } h2 { "Вернуться к чтению" } }
                            span { "{(progress.progress_fraction * 100.0).round() as u32}%" }
                        }
                        article { class: "continue-card",
                            div { class: "material-cover", aria_hidden: "true", span { "{material_format_short(&entry.kind)}" } strong { "{cover_monogram(entry.display_title())}" } }
                            div { class: "continue-copy",
                                p { class: "format-label", "{material_format_label(&entry.kind)}" }
                                h3 { "{entry.display_title()}" }
                                p { "Lumi откроет сохранённую позицию в общей версии материала." }
                                progress { max: "100", value: "{progress.progress_fraction * 100.0}", aria_label: "Прочитано {(progress.progress_fraction * 100.0).round() as u32}%" }
                                button { class: "primary-action", r#type: "button", onclick: move |_| on_open_reader.call(entry.id), "Продолжить чтение" }
                            }
                        }
                    }
                }
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
                                        match refresh_library(entries, continue_reading, refresh_generation).await {
                                            Ok(()) => {}
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

            section { class: "library-section telegram-connection", aria_label: "Подключение Telegram",
                div { class: "section-heading",
                    div {
                        p { class: "eyebrow", "Источник" }
                        h2 { "Telegram" }
                    }
                    span {
                        if telegram_status.read().as_ref().is_some_and(|status| status.connected) {
                            "Подключён"
                        } else if telegram_status.read().is_none() && telegram_error().is_empty() {
                            "Проверяем…"
                        } else {
                            "Не подключён"
                        }
                    }
                }
                if !capabilities_loaded {
                    p { class: "capability-note", role: "status", "Проверяем поддержку Telegram…" }
                } else if telegram_enabled {
                    p { "Привяжите личный чат, чтобы отправлять в Lumi текст, пересланные сообщения и одиночные web-ссылки. Текст и ограниченная атрибуция пересылки будут сохранены в вашем облачном аккаунте Lumi." }
                } else {
                    p { class: "capability-note", role: "status", "Этот сервер пока не поддерживает импорт из Telegram." }
                }
                if !telegram_error().is_empty() {
                    p { class: "account-error", role: "status", "{telegram_error}" }
                }
                if let Some(pairing) = telegram_pairing.read().as_ref() {
                    div { class: "library-alert", role: "status",
                        p { "Одноразовый токен действует 10 минут:" }
                        code { "{pairing.token}" }
                        if let Some(link) = pairing.deep_link.as_ref() {
                            a { class: "secondary-action", href: "{link}", target: "_blank", rel: "noopener noreferrer", "Открыть Telegram" }
                        }
                    }
                }
                if telegram_enabled { div { class: "material-actions",
                    if telegram_status.read().as_ref().is_some_and(|status| status.connected) {
                        button { class: "secondary-action", r#type: "button", disabled: telegram_busy(), onclick: move |_| {
                            let csrf = telegram_unlink_csrf.clone();
                            telegram_busy.set(true);
                            telegram_error.set(String::new());
                            spawn(async move {
                                match unlink_telegram(&csrf).await {
                                    Ok(()) => {
                                        telegram_pairing.set(None);
                                        telegram_status.set(Some(TelegramConnectionStatus {
                                            connected: false,
                                            telegram_user_id: None,
                                            linked_at: None,
                                            pairing_expires_at: None,
                                        }));
                                        match load_telegram_status().await {
                                            Ok(status) => telegram_status.set(Some(status)),
                                            Err(api_error) => telegram_error.set(api_error.to_string()),
                                        }
                                    }
                                    Err(api_error) => telegram_error.set(api_error.to_string()),
                                }
                                telegram_busy.set(false);
                            });
                        }, if telegram_busy() { "Отключаем…" } else { "Отключить Telegram" } }
                    } else {
                        button { class: "secondary-action", r#type: "button", disabled: telegram_busy() || telegram_pairing.read().is_some(), onclick: move |_| {
                            let csrf = telegram_pairing_csrf.clone();
                            telegram_busy.set(true);
                            telegram_error.set(String::new());
                            telegram_pairing.set(None);
                            spawn(async move {
                                match create_telegram_pairing(&csrf).await {
                                    Ok(pairing) => {
                                        let expires_at = pairing.expires_at;
                                        telegram_pairing.set(Some(pairing));
                                        telegram_busy.set(false);
                                        loop {
                                            browser_delay(2_000).await;
                                            if js_sys::Date::now() as u64 >= expires_at {
                                                telegram_pairing.set(None);
                                                telegram_error.set("Одноразовый токен истёк. Создайте новый.".to_owned());
                                                break;
                                            }
                                            match load_telegram_status().await {
                                                Ok(status) if status.connected => {
                                                    telegram_status.set(Some(status));
                                                    telegram_pairing.set(None);
                                                    telegram_error.set(String::new());
                                                    break;
                                                }
                                                Ok(status) => {
                                                    telegram_status.set(Some(status));
                                                    telegram_error.set(String::new());
                                                }
                                                Err(api_error) => {
                                                    telegram_error.set(format!("{} Повторяем проверку…", api_error));
                                                }
                                            }
                                        }
                                    }
                                    Err(api_error) => {
                                        telegram_error.set(api_error.to_string());
                                        telegram_busy.set(false);
                                    }
                                }
                            });
                        }, if telegram_busy() { "Создаём токен…" } else if telegram_pairing.read().is_some() { "Ожидаем Telegram…" } else { "Подключить Telegram" } }
                    }
                } }
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
                                        let _ = refresh_library(entries, continue_reading, refresh_generation).await;
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

        if add_open() {
            AddMaterialDialog {
                csrf_token: csrf_token.clone(),
                web_import_enabled,
                capabilities_loaded,
                on_close: move |_| {
                    add_open.set(false);
                    defer_account_focus("add-material-button");
                },
                on_accepted: move |accepted: AcceptedImport| {
                    spawn(async move {
                        let _ = refresh_library(entries, continue_reading, refresh_generation).await;
                        let _ = wait_for_job(accepted.job).await;
                        let _ = refresh_library(entries, continue_reading, refresh_generation).await;
                    });
                },
            }
        }

        if let Some(entry) = details.read().clone() {
            MaterialDetailsDialog { entry: entry.clone(), on_close: move |_| {
                details.set(None);
                defer_account_focus(&format!("details-{}", entry.id));
            } }
        }

        if let Some(entry) = delete_candidate.read().clone() {
            dialog { id: "delete-material-dialog", class: "library-dialog confirm-dialog", open: true, tabindex: "-1", aria_modal: "true", aria_label: "Удаление материала", oncancel: move |event| {
                event.prevent_default();
                let target = format!("delete-{}", entry.id);
                delete_candidate.set(None);
                defer_account_focus(&target);
            },
                p { class: "eyebrow danger-text", "Необратимо в интерфейсе" }
                h2 { "Удалить «{entry.display_title()}»?" }
                p { "Материал исчезнет из библиотеки. Сервер сохранит sync tombstone для согласованности реплик." }
                div { class: "dialog-actions",
                    button { class: "secondary-action", r#type: "button", onclick: move |_| {
                        let target = format!("delete-{}", entry.id);
                        delete_candidate.set(None);
                        defer_account_focus(&target);
                    }, "Отмена" }
                    button { class: "danger-action", r#type: "button", onclick: move |_| {
                        let csrf = csrf_token.clone();
                        let material_id = entry.id;
                        spawn(async move {
                            match delete_material(material_id, &csrf).await {
                                Ok(()) => {
                                    delete_candidate.set(None);
                                    let _ = refresh_library(entries, continue_reading, refresh_generation).await;
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
    let format_short = material_format_short(&entry.kind);
    let format_label = material_format_label(&entry.kind);
    let source_download_label = material_source_download_label(&entry.kind);
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
                span { "{format_short}" }
                strong { "{cover_monogram(&title)}" }
            }
            div { class: "material-copy",
                div { class: "material-card-heading",
                    div {
                        span { class: "format-label", "{format_label}" }
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
                    button { id: "details-{material_id}", class: "text-action", r#type: "button", onclick: move |_| on_details.call(details_entry.clone()), "Сведения" }
                    a { class: "text-action", href: "{API_BASE}/materials/{material_id}/source", "{source_download_label}" }
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
                    button { id: "delete-{material_id}", class: "text-action danger-text", r#type: "button", onclick: move |_| on_delete.call(delete_entry.clone()), "Удалить" }
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

#[derive(Clone, Copy, Eq, PartialEq)]
enum AddSourceMode {
    Epub,
    Web,
}

#[component]
fn AddMaterialDialog(
    csrf_token: String,
    web_import_enabled: bool,
    capabilities_loaded: bool,
    on_close: EventHandler<()>,
    on_accepted: EventHandler<AcceptedImport>,
) -> Element {
    let mut mode = use_signal(|| AddSourceMode::Epub);
    let mut selected = use_signal(|| Option::<SelectedEpub>::None);
    let mut url = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);
    use_effect(move || defer_account_dialog("add-material-dialog"));
    rsx! {
        dialog { id: "add-material-dialog", class: "library-dialog upload-dialog", open: true, tabindex: "-1", aria_modal: "true", aria_label: "Добавить материал", oncancel: move |event| {
            event.prevent_default();
            if !busy() { on_close.call(()); }
        }, onkeydown: move |event| if event.key() == Key::Escape && !busy() { on_close.call(()); },
            div { class: "dialog-heading",
                div {
                    p { class: "eyebrow", "Новый материал" }
                    h2 { "Добавить материал" }
                }
                button { class: "icon-action", r#type: "button", aria_label: "Закрыть загрузку", disabled: busy(), onclick: move |_| on_close.call(()), "×" }
            }
            div { class: "source-tabs", role: "tablist", aria_label: "Тип источника",
                button { id: "source-tab-epub", class: "secondary-action", r#type: "button", role: "tab", aria_selected: mode() == AddSourceMode::Epub, aria_controls: "source-panel-epub", tabindex: if mode() == AddSourceMode::Epub { "0" } else { "-1" }, onclick: move |_| mode.set(AddSourceMode::Epub), onkeydown: move |event| if matches!(event.key(), Key::ArrowRight | Key::ArrowLeft) && web_import_enabled { event.prevent_default(); mode.set(AddSourceMode::Web); focus_account_node("source-tab-web"); }, "EPUB" }
                button { id: "source-tab-web", class: "secondary-action", r#type: "button", role: "tab", aria_selected: mode() == AddSourceMode::Web, aria_controls: "source-panel-web", aria_disabled: !web_import_enabled, disabled: !web_import_enabled, tabindex: if mode() == AddSourceMode::Web { "0" } else { "-1" }, onclick: move |_| mode.set(AddSourceMode::Web), onkeydown: move |event| if matches!(event.key(), Key::ArrowRight | Key::ArrowLeft) { event.prevent_default(); mode.set(AddSourceMode::Epub); focus_account_node("source-tab-epub"); }, "Web-ссылка" }
            }
            if !capabilities_loaded {
                p { class: "capability-note", role: "status", "Проверяем поддержку импорта по URL…" }
            }
            if mode() == AddSourceMode::Epub {
                div { id: "source-panel-epub", role: "tabpanel", aria_labelledby: "source-tab-epub",
                p { "DRM-free reflowable EPUB до 100 MiB. Исходник сохраняется до запуска безопасного импортера." }
                label { class: "upload-dropzone",
                    span { class: "upload-icon", aria_hidden: "true", "＋" }
                    strong { if let Some(upload) = selected.read().as_ref() { "{upload.name}" } else { "Выберите файл EPUB" } }
                    small { if let Some(upload) = selected.read().as_ref() { "{upload.bytes.len()} байт" } else { ".epub · до 100 MiB" } }
                    input {
                        r#type: "file",
                        name: "epub_file",
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
                }
            } else {
                div { id: "source-panel-web", role: "tabpanel", aria_labelledby: "source-tab-web",
                p { "Укажите публичный HTTP(S) URL статьи. Lumi ограниченно загрузит HTML, сохранит snapshot и извлечёт основной текст." }
                label { class: "account-field",
                    span { "URL статьи" }
                    input {
                        r#type: "url",
                        name: "article_url",
                        autocomplete: "off",
                        value: "{url}",
                        placeholder: "https://example.org/article…",
                        disabled: busy(),
                        oninput: move |event| url.set(event.value()),
                    }
                }
                }
            }
            if !error().is_empty() {
                p { class: "account-error", role: "alert", "{error}" }
            }
            div { class: "dialog-actions",
                button { class: "secondary-action", r#type: "button", disabled: busy(), onclick: move |_| on_close.call(()), "Отмена" }
                button { class: "primary-action", r#type: "button", disabled: busy() || (mode() == AddSourceMode::Epub && selected.read().is_none()) || (mode() == AddSourceMode::Web && url().trim().is_empty()), onclick: move |_| {
                    let selected_upload = selected.read().clone();
                    let source_url = url();
                    let source_mode = mode();
                    let csrf = csrf_token.clone();
                    busy.set(true);
                    error.set(String::new());
                    spawn(async move {
                        let result = match source_mode {
                            AddSourceMode::Epub => match selected_upload.as_ref() {
                                Some(upload) => upload_epub(&csrf, upload).await,
                                None => return,
                            },
                            AddSourceMode::Web => import_web_url(&csrf, source_url.trim()).await,
                        };
                        match result {
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
    let download_label = material_source_download_label(&entry.kind);
    use_effect(move || defer_account_dialog("material-details-dialog"));
    rsx! {
        dialog { id: "material-details-dialog", class: "library-dialog details-dialog", open: true, tabindex: "-1", aria_modal: "true", aria_label: "Сведения о материале", oncancel: move |event| { event.prevent_default(); on_close.call(()); }, onkeydown: move |event| if event.key() == Key::Escape { on_close.call(()); },
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
                a { class: "secondary-action", href: "{API_BASE}/materials/{entry.id}/source", "{download_label}" }
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
        main { id: "main-content", class: "account-screen", aria_label: "Lumi — регистрация и вход",
            section { class: "account-card",
                p { class: "eyebrow", "Persistent account" }
                h1 { "Lumi" }
                p { "Seed phrase остаётся в браузере. Сервер хранит только публичный ключ и отзывную сессию." }
                div { class: "account-tabs", role: "tablist", aria_label: "Действие с аккаунтом",
                    button { id: "account-tab-register", r#type: "button", role: "tab", aria_selected: tab() == "register", aria_controls: "account-panel-register", tabindex: if tab() == "register" { "0" } else { "-1" }, onclick: move |_| tab.set("register".to_owned()), onkeydown: move |event| if matches!(event.key(), Key::ArrowRight | Key::ArrowLeft) { event.prevent_default(); tab.set("login".to_owned()); focus_account_node("account-tab-login"); }, "Создать аккаунт" }
                    button { id: "account-tab-login", r#type: "button", role: "tab", aria_selected: tab() == "login", aria_controls: "account-panel-login", tabindex: if tab() == "login" { "0" } else { "-1" }, onclick: move |_| tab.set("login".to_owned()), onkeydown: move |event| if matches!(event.key(), Key::ArrowRight | Key::ArrowLeft) { event.prevent_default(); tab.set("register".to_owned()); focus_account_node("account-tab-register"); }, "Войти / восстановить" }
                }
                if tab() == "register" {
                    div { id: "account-panel-register", role: "tabpanel", aria_labelledby: "account-tab-register",
                    label { class: "account-field",
                        span { "Псевдоним (необязательно)" }
                        input { name: "nickname", value: "{nickname}", maxlength: "80", autocomplete: "nickname", oninput: move |event| nickname.set(event.value()) }
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
                    }
                } else {
                    div { id: "account-panel-login", role: "tabpanel", aria_labelledby: "account-tab-login",
                    label { class: "account-field",
                        span { "Recovery phrase (24 слова)" }
                        textarea { name: "recovery_phrase", rows: "5", value: "{phrase}", autocomplete: "off", spellcheck: "false", placeholder: "Введите 24 слова…", oninput: move |event| phrase.set(event.value()) }
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

async fn refresh_library(
    mut entries: Signal<Option<Vec<LibraryEntry>>>,
    mut continue_reading: Signal<Option<(LibraryEntry, ReadingProgress)>>,
    mut generation: Signal<u64>,
) -> Result<(), ApiError> {
    generation += 1;
    let request_generation = generation();
    let loaded = load_materials().await?;
    if generation() != request_generation {
        return Ok(());
    }
    continue_reading.set(None);
    entries.set(Some(loaded));
    let projection = load_continue_reading().await.map_err(|error| {
        ApiError::Message(format!(
            "Библиотека загружена, но карточка продолжения недоступна: {error}"
        ))
    })?;
    if generation() == request_generation {
        continue_reading.set(projection);
    }
    Ok(())
}

async fn load_continue_reading() -> Result<Option<(LibraryEntry, ReadingProgress)>, ApiError> {
    let response = Request::get(&format!("{API_BASE}/materials/continue-reading"))
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(network_error)?;
    parse_json::<Option<ContinueReadingEntry>>(response)
        .await
        .map(|projection| projection.map(|value| (value.entry, value.progress)))
}

async fn load_capabilities() -> Result<ServiceCapabilities, ApiError> {
    let response = Request::get(&format!("{API_BASE}/capabilities"))
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

async fn import_web_url(csrf: &str, url: &str) -> Result<AcceptedImport, ApiError> {
    let request = Request::post(&format!("{API_BASE}/imports/url"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .header("Idempotency-Key", &Uuid::now_v7().to_string())
        .json(&ImportWebUrlRequest {
            url: url.to_owned(),
        })
        .map_err(network_error)?;
    parse_json(request.send().await.map_err(network_error)?).await
}

async fn load_telegram_status() -> Result<TelegramConnectionStatus, ApiError> {
    let response = Request::get(&format!("{API_BASE}/providers/telegram/connection"))
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(network_error)?;
    parse_json(response).await
}

async fn load_telegram_bot_settings() -> Result<TelegramBotSettings, ApiError> {
    let response = Request::get(&format!("{API_BASE}/settings/telegram"))
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(network_error)?;
    parse_json(response).await
}

async fn update_telegram_bot_token(
    csrf: &str,
    token: &str,
) -> Result<TelegramBotSettings, ApiError> {
    let request = Request::put(&format!("{API_BASE}/settings/telegram/token"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .json(&UpdateTelegramBotTokenRequest {
            token: token.to_owned(),
        })
        .map_err(network_error)?;
    parse_json(request.send().await.map_err(network_error)?).await
}

async fn delete_telegram_bot_token(csrf: &str) -> Result<TelegramBotSettings, ApiError> {
    let response = Request::delete(&format!("{API_BASE}/settings/telegram/token"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .send()
        .await
        .map_err(network_error)?;
    parse_json(response).await
}

async fn create_telegram_pairing(csrf: &str) -> Result<TelegramPairingResponse, ApiError> {
    let response = Request::post(&format!("{API_BASE}/providers/telegram/pairing"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .send()
        .await
        .map_err(network_error)?;
    parse_json(response).await
}

async fn unlink_telegram(csrf: &str) -> Result<(), ApiError> {
    let response = Request::delete(&format!("{API_BASE}/providers/telegram/connection"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .send()
        .await
        .map_err(network_error)?;
    if response.ok() {
        Ok(())
    } else {
        Err(api_response_error(&response))
    }
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
        lumi_core::JobStage::FetchingSource => "Загружаем страницу",
        lumi_core::JobStage::CapturingSnapshot => "Сохраняем snapshot",
        lumi_core::JobStage::ExtractingContent => "Извлекаем основной текст",
        lumi_core::JobStage::ValidatingContainer => "Проверяем контейнер",
        lumi_core::JobStage::Normalizing => "Нормализуем главы",
        lumi_core::JobStage::Persisting => "Публикуем результат",
        lumi_core::JobStage::ReaderDocumentBuilt => "Готовим документ чтения",
        lumi_core::JobStage::Committed => "Импорт завершён",
    }
}

fn material_format_short(kind: &MaterialKind) -> &'static str {
    match kind {
        MaterialKind::Epub => "EPUB",
        MaterialKind::WebPage => "WEB",
        MaterialKind::Telegram => "TG",
    }
}

fn material_format_label(kind: &MaterialKind) -> &'static str {
    match kind {
        MaterialKind::Epub => "EPUB · книга",
        MaterialKind::WebPage => "Web · статья",
        MaterialKind::Telegram => "Telegram · текст",
    }
}

fn material_source_download_label(kind: &MaterialKind) -> &'static str {
    match kind {
        MaterialKind::Epub => "Скачать исходник",
        MaterialKind::WebPage => "Скачать snapshot",
        MaterialKind::Telegram => "Скачать сохранённый текст",
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
        notify_session_expired();
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

fn clear_csrf_cookie() {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Ok(document) = document.dyn_into::<web_sys::HtmlDocument>() else {
        return;
    };
    let _ = document.set_cookie("lumi_csrf=; Path=/; Max-Age=0; SameSite=Strict");
}

fn focus_account_node(id: &str) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Some(element) = document.get_element_by_id(id) else {
        return;
    };
    if let Ok(element) = element.dyn_into::<web_sys::HtmlElement>() {
        let _ = element.focus();
    }
}

fn defer_account_focus(id: &str) {
    let id = id.to_owned();
    spawn(async move {
        browser_delay(20).await;
        focus_account_node(&id);
    });
}

fn defer_account_dialog(id: &str) {
    let id = id.to_owned();
    spawn(async move {
        browser_delay(20).await;
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        let Some(element) = document.get_element_by_id(&id) else {
            return;
        };
        let Ok(dialog) = element.dyn_into::<web_sys::HtmlDialogElement>() else {
            return;
        };
        if dialog.open() {
            dialog.close();
        }
        let _ = dialog.show_modal();
        let _ = dialog.focus();
    });
}

fn network_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::Message(format!("Сеть/API недоступны: {error}"))
}

fn contract_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::Message(format!("Auth challenge отклонён: {error}"))
}
