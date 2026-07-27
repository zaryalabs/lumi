//! Persistent account gate and the server-backed Stage 3 library.

use bip39::{Language, Mnemonic};
use dioxus::prelude::*;
use gloo_net::http::Request;
use lumi_core::{
    decode_auth_bytes, encode_auth_bytes, AcceptedImport, AccountSummary, AiPage, AuthChallenge,
    ChallengeResponse, CompleteLoginRequest, ContinueReadingEntry, CreateChallengeRequest,
    CreateMcpConnectionRequest, DerivedAuthMaterial, ImportWebUrlRequest, InstanceRole, Job,
    JobStatus, LibraryEntry, LibraryState, MaterialImportStatus, MaterialKind, McpConnection,
    McpConnectionStatus, McpConnectionTokenResponse, ReadingProgress, RegisterAccountRequest,
    RevokeMcpConnectionRequest, ServiceCapabilities, SessionBootstrap, TelegramBotRuntimeStatus,
    TelegramBotSettings, UpdateLibraryStateCommand, UpdateTelegramBotTokenRequest,
};
use uuid::Uuid;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::RequestCredentials;

use crate::routing::{
    browser_requests_system_settings, initial_route, set_browser_route, AppRoute, DeskRoute,
    SearchRoute,
};

pub(crate) const API_BASE: &str = match option_env!("LUMI_API_BASE") {
    Some(value) => value,
    None => "/api/v1",
};

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
    let mut service_capabilities = use_signal(|| Option::<ServiceCapabilities>::None);
    let mut bootstrap_generation = use_signal(|| 0_u64);
    use_effect(move || {
        let Some(window) = web_sys::window() else {
            return;
        };
        let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            clear_csrf_cookie();
            csrf.set(String::new());
            service_capabilities.set(None);
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
        let Some(window) = web_sys::window() else {
            return;
        };
        let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            route.set(initial_route());
        });
        let _ =
            window.add_event_listener_with_callback("hashchange", handler.as_ref().unchecked_ref());
        handler.forget();
    });
    use_effect(move || {
        let title = match route() {
            AppRoute::Library => "Библиотека — Lumi",
            AppRoute::Challenges => "Челленджи — Lumi",
            AppRoute::AiQueue => "AI-задачи — Lumi",
            AppRoute::Connections => "Подключения — Lumi",
            AppRoute::Settings => "Администрирование — Lumi",
            AppRoute::Reader(_, _, _) => "Чтение — Lumi",
            AppRoute::LearningSession(_) => "Самопроверка — Lumi",
            AppRoute::MaterialLearning(_, _) => "Обучение — Lumi",
            AppRoute::Desk(_) => "Desk — Lumi",
            AppRoute::Search(_) => "Поиск — Lumi",
        };
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            document.set_title(title);
        }
    });
    use_effect(move || {
        let needs_record_capability = matches!(
            route(),
            AppRoute::Reader(_, _, _) | AppRoute::Desk(_) | AppRoute::Search(_)
        );
        let signed_in = matches!(&*state.read(), AccountState::SignedIn(_));
        if signed_in && needs_record_capability {
            spawn(async move {
                service_capabilities.set(load_capabilities().await.ok());
            });
        }
    });
    use_effect(move || {
        let _ = bootstrap_generation();
        spawn(async move {
            match load_account().await {
                Ok(account) => {
                    csrf.set(read_cookie("lumi_csrf").unwrap_or_default());
                    if account.instance_role != InstanceRole::Admin
                        && (route() == AppRoute::Settings || browser_requests_system_settings())
                    {
                        set_browser_route(&AppRoute::Library);
                        route.set(AppRoute::Library);
                    }
                    state.set(AccountState::SignedIn(account));
                }
                Err(ApiError::Unauthorized) => {
                    service_capabilities.set(None);
                    state.set(AccountState::SignedOut);
                }
                Err(error) => state.set(AccountState::Failed(error.to_string())),
            }
        });
    });

    let account_state = state.read().clone();
    let record_rag_enabled = service_capabilities
        .read()
        .as_ref()
        .is_some_and(|capabilities| {
            capabilities
                .features
                .iter()
                .any(|feature| feature == "record-rag")
        });
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
                    if session.account.instance_role != InstanceRole::Admin
                        && (route() == AppRoute::Settings || browser_requests_system_settings())
                    {
                        set_browser_route(&AppRoute::Library);
                        route.set(AppRoute::Library);
                    }
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
            let is_admin = account.instance_role == InstanceRole::Admin;
            let csrf_for_logout = csrf.read().clone();
            let account_label = account
                .nickname
                .as_deref()
                .unwrap_or("без псевдонима")
                .to_owned();
            rsx! {
                div { class: "library-app",
                    a { class: "skip-link", href: "#main-content", "Перейти к содержанию" }
                    if !matches!(route(), AppRoute::Reader(_, _, _) | AppRoute::LearningSession(_)) {
                    header { class: "library-topbar",
                        a { class: "library-brand", href: "#library", aria_label: "Lumi — библиотека", onclick: move |_| {
                            set_browser_route(&AppRoute::Library);
                            route.set(AppRoute::Library);
                        },
                            span { class: "brand-mark", aria_hidden: "true", "L" }
                            strong { "Lumi" }
                        }
                        nav { aria_label: "Основная навигация",
                            a { href: "#library", aria_current: if route() == AppRoute::Library { "page" } else { "false" }, onclick: move |_| {
                                set_browser_route(&AppRoute::Library);
                                route.set(AppRoute::Library);
                            }, "Библиотека" }
                            a { href: "#desk", aria_current: if matches!(route(), AppRoute::Desk(_)) { "page" } else { "false" }, onclick: move |_| {
                                let next = AppRoute::Desk(DeskRoute::default());
                                set_browser_route(&next);
                                route.set(next);
                            }, "Desk" }
                            a { href: "#search", aria_current: if matches!(route(), AppRoute::Search(_)) { "page" } else { "false" }, onclick: move |_| {
                                let next = AppRoute::Search(SearchRoute::default());
                                set_browser_route(&next);
                                route.set(next);
                            }, "Поиск" }
                            a { href: "#challenges", aria_current: if route() == AppRoute::Challenges { "page" } else { "false" }, onclick: move |_| {
                                set_browser_route(&AppRoute::Challenges);
                                route.set(AppRoute::Challenges);
                            }, "Челленджи" }
                            a { href: "#ai-queue", aria_current: if route() == AppRoute::AiQueue { "page" } else { "false" }, onclick: move |_| {
                                set_browser_route(&AppRoute::AiQueue);
                                route.set(AppRoute::AiQueue);
                            }, "AI-задачи" }
                            a { href: "#connections", aria_current: if route() == AppRoute::Connections { "page" } else { "false" }, onclick: move |_| {
                                set_browser_route(&AppRoute::Connections);
                                route.set(AppRoute::Connections);
                            }, "Подключения" }
                            if is_admin {
                                a { href: "#settings", aria_current: if route() == AppRoute::Settings { "page" } else { "false" }, onclick: move |_| {
                                    set_browser_route(&AppRoute::Settings);
                                    route.set(AppRoute::Settings);
                                }, "Администрирование" }
                            }
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
                                            service_capabilities.set(None);
                                        }
                                    });
                                },
                                "Выйти"
                            }
                        }
                    }
                    }
                    if let AppRoute::Reader(material_id, return_to, anchor) = route() {
                        crate::pdf_reader::ReaderRoute {
                            material_id,
                            initial_anchor: anchor,
                            csrf_token: csrf.read().clone(),
                            record_rag_enabled,
                            on_close: move |_| {
                                let next = return_to.map_or(AppRoute::Library, AppRoute::LearningSession);
                                set_browser_route(&next);
                                route.set(next);
                            },
                            on_open_learning_session: move |session_id| {
                                let next = AppRoute::LearningSession(session_id);
                                set_browser_route(&next);
                                route.set(next);
                            },
                            on_manage_learning: move |(material_id, source_id)| {
                                let next = AppRoute::MaterialLearning(material_id, Some(source_id));
                                set_browser_route(&next);
                                route.set(next);
                            },
                        }
                    } else if let AppRoute::LearningSession(session_id) = route() {
                        crate::learning::LearningSessionPage {
                            session_id,
                            csrf_token: csrf.read().clone(),
                            on_open_source: move |(material_id, session_id)| {
                                let next = AppRoute::Reader(material_id, Some(session_id), None);
                                set_browser_route(&next);
                                route.set(next);
                            },
                            on_close: move |_| {
                                set_browser_route(&AppRoute::Library);
                                route.set(AppRoute::Library);
                            },
                        }
                    } else if let AppRoute::MaterialLearning(material_id, source_id) = route() {
                        crate::learning::MaterialLearningPage {
                            material_id,
                            source_id,
                            csrf_token: csrf.read().clone(),
                            on_open_session: move |session_id| {
                                let next = AppRoute::LearningSession(session_id);
                                set_browser_route(&next);
                                route.set(next);
                            },
                        }
                    } else if route() == AppRoute::Connections {
                        ConnectionsApp { csrf_token: csrf.read().clone() }
                    } else if route() == AppRoute::Challenges {
                        crate::learning::ChallengesPage {
                            csrf_token: csrf.read().clone(),
                            on_open_session: move |session_id| {
                                let next = AppRoute::LearningSession(session_id);
                                set_browser_route(&next);
                                route.set(next);
                            },
                        }
                    } else if route() == AppRoute::AiQueue {
                        crate::ai::AiQueuePage { csrf_token: csrf.read().clone() }
                    } else if let AppRoute::Desk(desk_route) = route() {
                        crate::desk::DeskPage {
                            route: desk_route,
                            csrf_token: csrf.read().clone(),
                            record_rag_enabled,
                            on_route: move |next| {
                                set_browser_route(&next);
                                route.set(next);
                            },
                        }
                    } else if let AppRoute::Search(search_route) = route() {
                        crate::search_ui::GlobalSearchPage {
                            route: search_route,
                            record_rag_enabled,
                            on_open: move |target| crate::search_ui::open_search_target(&target),
                        }
                    } else if route() == AppRoute::Settings && is_admin {
                        SettingsApp { csrf_token: csrf.read().clone() }
                    } else {
                        LibraryApp {
                            csrf_token: csrf.read().clone(),
                            on_open_reader: move |material_id| {
                                let next = AppRoute::Reader(material_id, None, None);
                                set_browser_route(&next);
                                route.set(next);
                            },
                            on_open_learning: move |material_id| {
                                let next = AppRoute::MaterialLearning(material_id, None);
                                set_browser_route(&next);
                                route.set(next);
                            },
                            on_search: move |query| {
                                let next = AppRoute::Search(SearchRoute {
                                    query,
                                    ..SearchRoute::default()
                                });
                                set_browser_route(&next);
                                route.set(next);
                            },
                        }
                    }
                    crate::ai::GlobalAiChat { csrf_token: csrf.read().clone() }
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
    let mut disconnect_open = use_signal(|| false);

    use_effect(move || {
        spawn(async move {
            match load_telegram_bot_settings().await {
                Ok(value) => settings.set(Some(value)),
                Err(load_error) => error.set(load_error.to_string()),
            }
        });
    });
    use_effect(move || {
        if disconnect_open() {
            defer_account_dialog("disconnect-telegram-bot-dialog");
        }
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
                    h1 { "Системные настройки" }
                    p { class: "library-lead", "Подключения и параметры этого экземпляра Lumi, доступные администратору." }
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
                    "Это глобальная настройка сервера. Изменения применяются ко всем пользователям экземпляра Lumi."
                }

                if let Some(current) = settings_snapshot.as_ref() {
                    if current.configured {
                        dl { class: "settings-summary",
                            div { dt { "Бот" } dd { "{bot_label}" } }
                            div { dt { "Bot ID" } dd { "{bot_id_label}" } }
                            div { dt { "Токен" } dd { "{fingerprint_label}" } }
                        }
                        p { class: "capability-note",
                            "Отдельное подтверждение не требуется: откройте бота и сразу отправьте материал. Первый личный чат привяжется к этому аккаунту администратора автоматически."
                        }
                        if let Some(username) = current.bot_username.as_ref() {
                            a {
                                class: "secondary-action",
                                href: "https://t.me/{username}",
                                target: "_blank",
                                rel: "noopener noreferrer",
                                "Открыть Telegram"
                            }
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
                        button {
                            id: "disconnect-telegram-bot",
                            class: "danger-action",
                            r#type: "button",
                            disabled: busy(),
                            onclick: move |_| disconnect_open.set(true),
                            "Отключить бота"
                        }
                    }
                }
            }
        }

        if disconnect_open() {
            dialog {
                id: "disconnect-telegram-bot-dialog",
                class: "library-dialog confirm-dialog",
                open: true,
                tabindex: "-1",
                aria_modal: "true",
                aria_label: "Отключение Telegram-бота",
                oncancel: move |event| {
                    event.prevent_default();
                    disconnect_open.set(false);
                    defer_account_focus("disconnect-telegram-bot");
                },
                p { class: "eyebrow danger-text", "Для всего сервера" }
                h2 { "Отключить Telegram-бота?" }
                p { "Пользователи больше не смогут отправлять материалы через Telegram, пока администратор не подключит бота снова." }
                div { class: "dialog-actions",
                    button { class: "secondary-action", r#type: "button", onclick: move |_| {
                        disconnect_open.set(false);
                        defer_account_focus("disconnect-telegram-bot");
                    }, "Отмена" }
                    button { class: "danger-action", r#type: "button", disabled: busy(), onclick: move |_| {
                        let csrf = delete_csrf.clone();
                        busy.set(true);
                        error.set(String::new());
                        spawn(async move {
                            match delete_telegram_bot_token(&csrf).await {
                                Ok(value) => {
                                    token.set(String::new());
                                    settings.set(Some(value));
                                    disconnect_open.set(false);
                                }
                                Err(delete_error) => error.set(delete_error.to_string()),
                            }
                            busy.set(false);
                        });
                    }, if busy() { "Отключаем…" } else { "Отключить" } }
                }
            }
        }
    }
}

#[component]
fn ConnectionsApp(csrf_token: String) -> Element {
    let mut error = use_signal(String::new);
    let mut capabilities = use_signal(|| Option::<ServiceCapabilities>::None);
    let mut mcp_connections = use_signal(|| Option::<AiPage<McpConnection>>::None);
    let mut connection_name = use_signal(String::new);
    let mut one_time_token = use_signal(|| Option::<McpConnectionTokenResponse>::None);
    let mut busy = use_signal(|| false);

    use_effect(move || {
        spawn(async move {
            match load_capabilities().await {
                Ok(value) => capabilities.set(Some(value)),
                Err(api_error) => error.set(format!(
                    "Не удалось проверить возможности сервера: {api_error}"
                )),
            }
            match load_mcp_connections().await {
                Ok(value) => mcp_connections.set(Some(value)),
                Err(api_error) => {
                    error.set(format!("Не удалось загрузить MCP-подключения: {api_error}"))
                }
            }
        });
    });
    let capabilities_loaded = capabilities.read().is_some();
    let telegram_enabled = capabilities.read().as_ref().is_some_and(|value| {
        value
            .features
            .iter()
            .any(|feature| feature == "telegram-text-import")
            && value
                .features
                .iter()
                .any(|feature| feature == "telegram-admin-auto-link")
    });
    let mcp_enabled = capabilities.read().as_ref().is_some_and(|value| {
        value
            .features
            .iter()
            .any(|feature| feature == "mcp-account-agent")
    });
    let connections = mcp_connections
        .read()
        .as_ref()
        .map(|page| page.items.clone())
        .unwrap_or_default();
    let create_csrf = csrf_token.clone();

    rsx! {
        main { id: "main-content", class: "library-view connections-view", aria_label: "Личные подключения",
            header { class: "library-hero compact",
                div {
                    p { class: "eyebrow", "Личный аккаунт" }
                    h1 { "Подключения" }
                    p { class: "library-lead", "Здесь находятся внешние сервисы, связанные только с вашим аккаунтом Lumi." }
                }
            }

            section { class: "library-section telegram-connection", aria_label: "Подключение Telegram",
                div { class: "section-heading",
                    div {
                        p { class: "eyebrow", "Источник материалов" }
                        h2 { "Telegram" }
                    }
                    span {
                        if !capabilities_loaded {
                            "Проверяем…"
                        } else if !telegram_enabled {
                            "Недоступен"
                        } else {
                            "Без отдельной привязки"
                        }
                    }
                }

                if !capabilities_loaded {
                    p { class: "capability-note", role: "status", "Проверяем поддержку Telegram…" }
                } else if telegram_enabled {
                    p { "Если администратор добавил Telegram-бота, отдельный код подключения не нужен. Откройте бота и сразу отправьте или перешлите текст, фотографию либо публичную ссылку. Первый личный чат будет привязан автоматически." }
                } else {
                    p { class: "capability-note", role: "status", "Импорт из Telegram не включён на этом сервере. Обратитесь к администратору Lumi." }
                }

                if !error().is_empty() {
                    div { class: "library-alert", role: "alert",
                        span { "{error}" }
                        button { r#type: "button", onclick: move |_| {
                            error.set(String::new());
                            spawn(async move {
                                match load_capabilities().await {
                                    Ok(value) => capabilities.set(Some(value)),
                                    Err(api_error) => error.set(format!("Не удалось проверить возможности сервера: {api_error}")),
                                }
                            });
                        }, "Повторить" }
                    }
                }
            }

            section { class: "library-section", aria_label: "Подключения внешних агентов MCP",
                div { class: "section-heading",
                    div {
                        p { class: "eyebrow", "Внешние агенты" }
                        h2 { "MCP" }
                    }
                    span {
                        if !capabilities_loaded {
                            "Проверяем…"
                        } else if mcp_enabled {
                            "{connections.iter().filter(|item| item.status == McpConnectionStatus::Active).count()} активно"
                        } else {
                            "Недоступен"
                        }
                    }
                }
                p { "Создайте отдельный отзываемый токен для Codex или другого MCP-клиента. Агент получит доступ только к продуктовым данным этого аккаунта — без ключей провайдера, администрирования и внутреннего чата." }

                if let Some(created) = one_time_token.read().as_ref() {
                    div { class: "library-alert", role: "status", aria_label: "Новый MCP-токен",
                        div {
                            strong { "Скопируйте токен сейчас — повторно он не показывается." }
                            p { "Endpoint: {created.endpoint}" }
                            code { "{created.token}" }
                        }
                        button { r#type: "button", onclick: move |_| one_time_token.set(None), "Скрыть" }
                    }
                }

                if mcp_enabled {
                    div { class: "material-actions",
                        label { class: "account-field",
                            span { "Название подключения" }
                            input {
                                r#type: "text",
                                name: "mcp_connection_name",
                                maxlength: "120",
                                placeholder: "Например, Codex на ноутбуке",
                                value: "{connection_name}",
                                oninput: move |event| connection_name.set(event.value()),
                            }
                        }
                        button {
                            class: "primary-action",
                            r#type: "button",
                            disabled: busy() || connection_name().trim().is_empty(),
                            onclick: move |_| {
                                let name = connection_name.read().clone();
                                let csrf = create_csrf.clone();
                                busy.set(true);
                                error.set(String::new());
                                spawn(async move {
                                    match create_mcp_connection(&csrf, &name).await {
                                        Ok(created) => {
                                            one_time_token.set(Some(created));
                                            connection_name.set(String::new());
                                            match load_mcp_connections().await {
                                                Ok(value) => mcp_connections.set(Some(value)),
                                                Err(api_error) => error.set(api_error.to_string()),
                                            }
                                        }
                                        Err(api_error) => error.set(api_error.to_string()),
                                    }
                                    busy.set(false);
                                });
                            },
                            if busy() { "Создаём…" } else { "Создать подключение" }
                        }
                    }
                }

                if mcp_connections.read().is_none() {
                    p { class: "capability-note", role: "status", "Загружаем подключения…" }
                } else if connections.is_empty() {
                    p { class: "capability-note", "MCP-подключений пока нет." }
                } else {
                    div { class: "material-grid", aria_label: "Список MCP-подключений",
                        for connection in connections {
                            article { class: "material-card", key: "{connection.id}",
                                div { class: "material-card-body",
                                    p { class: "format-label", "MCP · {connection.token_fingerprint}" }
                                    h3 { "{connection.name}" }
                                    p {
                                        if connection.status == McpConnectionStatus::Active {
                                            "Активно"
                                        } else {
                                            "Отозвано"
                                        }
                                    }
                                }
                                div { class: "material-actions",
                                    button {
                                        class: "secondary-action",
                                        r#type: "button",
                                        disabled: busy(),
                                        onclick: {
                                            let csrf = csrf_token.clone();
                                            let connection = connection.clone();
                                            move |_| {
                                                let csrf = csrf.clone();
                                                let connection = connection.clone();
                                                busy.set(true);
                                                error.set(String::new());
                                                spawn(async move {
                                                    match rotate_mcp_connection(&csrf, &connection).await {
                                                        Ok(created) => {
                                                            one_time_token.set(Some(created));
                                                            if let Ok(value) = load_mcp_connections().await {
                                                                mcp_connections.set(Some(value));
                                                            }
                                                        }
                                                        Err(api_error) => error.set(api_error.to_string()),
                                                    }
                                                    busy.set(false);
                                                });
                                            }
                                        },
                                        "Ротировать токен"
                                    }
                                    if connection.status == McpConnectionStatus::Active {
                                        button {
                                            class: "danger-action",
                                            r#type: "button",
                                            disabled: busy(),
                                            onclick: {
                                                let csrf = csrf_token.clone();
                                                let connection = connection.clone();
                                                move |_| {
                                                    let csrf = csrf.clone();
                                                    let connection = connection.clone();
                                                    busy.set(true);
                                                    error.set(String::new());
                                                    spawn(async move {
                                                        match revoke_mcp_connection(&csrf, &connection).await {
                                                            Ok(()) => {
                                                                if let Ok(value) = load_mcp_connections().await {
                                                                    mcp_connections.set(Some(value));
                                                                }
                                                            }
                                                            Err(api_error) => error.set(api_error.to_string()),
                                                        }
                                                        busy.set(false);
                                                    });
                                                }
                                            },
                                            "Отозвать"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn LibraryApp(
    csrf_token: String,
    on_open_reader: EventHandler<Uuid>,
    on_open_learning: EventHandler<Uuid>,
    on_search: EventHandler<String>,
) -> Element {
    let entries = use_signal(|| Option::<Vec<LibraryEntry>>::None);
    let mut error = use_signal(String::new);
    let mut add_open = use_signal(|| false);
    let mut details = use_signal(|| Option::<LibraryEntry>::None);
    let mut delete_candidate = use_signal(|| Option::<LibraryEntry>::None);
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
    let capabilities_loaded = capabilities.read().is_some();
    let web_import_enabled = capabilities.read().as_ref().is_some_and(|value| {
        value
            .features
            .iter()
            .any(|feature| feature == "public-web-url-import")
    });
    let pdf_import_enabled = capabilities.read().as_ref().is_some_and(|value| {
        value
            .features
            .iter()
            .any(|feature| feature == "pdf-fixed-layout-import")
    });
    let markdown_import_enabled = capabilities.read().as_ref().is_some_and(|value| {
        value
            .features
            .iter()
            .any(|feature| feature == "markdown-import")
    });
    let lum_import_enabled = capabilities
        .read()
        .as_ref()
        .is_some_and(|value| value.features.iter().any(|feature| feature == "lum-import"));
    let abridgement_enabled = capabilities.read().as_ref().is_some_and(|value| {
        value
            .features
            .iter()
            .any(|feature| feature == "ai-abridged-lum")
    });
    rsx! {
        main { id: "main-content", class: "library-view", aria_label: "Библиотека Lumi",
            header { class: if loaded { "library-hero compact" } else { "library-hero" },
                div {
                    p { class: "eyebrow", "Личное пространство" }
                    h1 { "Ваша библиотека" }
                    p { class: "library-lead", "Книги, документы и статьи — в одном месте, с сохранённой позицией чтения." }
                }
                button {
                    id: "add-material-button",
                    class: "primary-action add-material",
                    r#type: "button",
                    onclick: move |_| add_open.set(true),
                    "＋ Добавить материал"
                }
            }
            crate::search_ui::LibrarySearch { on_submit: on_search }

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
                    p { "Добавьте EPUB без защиты, LUM, Markdown, PDF или публичную статью по ссылке." }
                    button { class: "primary-action", r#type: "button", onclick: move |_| add_open.set(true), "Добавить материал" }
                }
            } else {
                if let Some((entry, progress)) = continue_reading.read().clone() {
                    section { class: "continue-section", aria_label: "Продолжить чтение",
                        div { class: "section-heading",
                            div { p { class: "eyebrow", "Продолжить" } h2 { "Вернуться к чтению" } }
                            span { "{reading_progress_label(progress.progress_fraction)}" }
                        }
                        article { class: "continue-card",
                            div { class: "material-cover", aria_hidden: "true", span { "{material_format_short(&entry.kind)}" } strong { "{cover_monogram(entry.display_title())}" } }
                            div { class: "continue-copy",
                                p { class: "format-label", "{material_format_label(&entry.kind)}" }
                                h3 { "{entry.display_title()}" }
                                p { "Откроем материал на последней сохранённой позиции." }
                                progress { max: "100", value: "{progress.progress_fraction * 100.0}", aria_label: "Прочитано {reading_progress_label(progress.progress_fraction)}" }
                                button { class: "primary-action", r#type: "button", onclick: move |_| on_open_reader.call(entry.id), "Продолжить чтение" }
                            }
                        }
                    }
                }
                section { class: "library-section", aria_label: "Активные материалы",
                    div { class: "section-heading",
                        div {
                            p { class: "eyebrow", "Библиотека" }
                            h2 { "Все материалы" }
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
                                on_open_learning,
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
                                        let _ = refresh_library(entries, continue_reading, refresh_generation).await;
                                    });
                                },
                                on_details: move |entry| details.set(Some(entry)),
                                on_delete: move |entry| delete_candidate.set(Some(entry)),
                                on_open_reader,
                                on_open_learning,
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
                pdf_import_enabled,
                markdown_import_enabled,
                lum_import_enabled,
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
            MaterialDetailsDialog { entry: entry.clone(), csrf_token: csrf_token.clone(), abridgement_enabled, on_close: move |_| {
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
                p { class: "eyebrow danger-text", "Удаление" }
                h2 { "Удалить «{entry.display_title()}»?" }
                p { "Материал исчезнет из библиотеки на всех ваших устройствах. Это действие нельзя отменить." }
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
    on_open_learning: EventHandler<Uuid>,
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
                        if entry.derivation.is_some() {
                            span { class: "status-pill ready", "Производный материал" }
                        }
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
                        button { class: "secondary-action", r#type: "button", onclick: move |_| on_open_learning.call(material_id), "Учиться" }
                    }
                    details { class: "material-more",
                        summary {
                            role: "button",
                            aria_label: "Дополнительные действия с материалом",
                            "Ещё"
                        }
                        div { class: "material-menu-actions",
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
                                }, "Отменить импорт" }
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
                                }, "Повторить импорт" }
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
                            }, if archived { "Вернуть в библиотеку" } else { "Переместить в архив" } }
                            button { id: "delete-{material_id}", class: "text-action danger-text", r#type: "button", onclick: move |_| on_delete.call(delete_entry.clone()), "Удалить" }
                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone)]
struct SelectedUpload {
    name: String,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum AddSourceMode {
    Epub,
    Lum,
    Markdown,
    Pdf,
    Web,
}

#[component]
fn AddMaterialDialog(
    csrf_token: String,
    web_import_enabled: bool,
    pdf_import_enabled: bool,
    markdown_import_enabled: bool,
    lum_import_enabled: bool,
    capabilities_loaded: bool,
    on_close: EventHandler<()>,
    on_accepted: EventHandler<AcceptedImport>,
) -> Element {
    let mut mode = use_signal(|| AddSourceMode::Epub);
    let mut selected = use_signal(|| Option::<SelectedUpload>::None);
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
                button { id: "source-tab-epub", class: "secondary-action", r#type: "button", role: "tab", aria_selected: mode() == AddSourceMode::Epub, aria_controls: "source-panel-epub", tabindex: if mode() == AddSourceMode::Epub { "0" } else { "-1" }, onclick: move |_| { mode.set(AddSourceMode::Epub); selected.set(None); }, onkeydown: move |event| if event.key() == Key::ArrowRight && lum_import_enabled { event.prevent_default(); mode.set(AddSourceMode::Lum); selected.set(None); defer_account_focus("source-tab-lum"); }, "EPUB" }
                button { id: "source-tab-lum", class: "secondary-action", r#type: "button", role: "tab", aria_selected: mode() == AddSourceMode::Lum, aria_controls: "source-panel-lum", aria_disabled: !lum_import_enabled, disabled: !lum_import_enabled, tabindex: if mode() == AddSourceMode::Lum { "0" } else { "-1" }, onclick: move |_| { mode.set(AddSourceMode::Lum); selected.set(None); }, onkeydown: move |event| if event.key() == Key::ArrowLeft { event.prevent_default(); mode.set(AddSourceMode::Epub); selected.set(None); defer_account_focus("source-tab-epub"); } else if event.key() == Key::ArrowRight && markdown_import_enabled { event.prevent_default(); mode.set(AddSourceMode::Markdown); selected.set(None); defer_account_focus("source-tab-markdown"); }, "LUM" }
                button { id: "source-tab-markdown", class: "secondary-action", r#type: "button", role: "tab", aria_selected: mode() == AddSourceMode::Markdown, aria_controls: "source-panel-markdown", aria_disabled: !markdown_import_enabled, disabled: !markdown_import_enabled, tabindex: if mode() == AddSourceMode::Markdown { "0" } else { "-1" }, onclick: move |_| { mode.set(AddSourceMode::Markdown); selected.set(None); }, onkeydown: move |event| if event.key() == Key::ArrowLeft && lum_import_enabled { event.prevent_default(); mode.set(AddSourceMode::Lum); selected.set(None); defer_account_focus("source-tab-lum"); } else if event.key() == Key::ArrowRight && pdf_import_enabled { event.prevent_default(); mode.set(AddSourceMode::Pdf); selected.set(None); defer_account_focus("source-tab-pdf"); }, "Markdown" }
                button { id: "source-tab-pdf", class: "secondary-action", r#type: "button", role: "tab", aria_selected: mode() == AddSourceMode::Pdf, aria_controls: "source-panel-pdf", aria_disabled: !pdf_import_enabled, disabled: !pdf_import_enabled, tabindex: if mode() == AddSourceMode::Pdf { "0" } else { "-1" }, onclick: move |_| { mode.set(AddSourceMode::Pdf); selected.set(None); }, onkeydown: move |event| if event.key() == Key::ArrowLeft && markdown_import_enabled { event.prevent_default(); mode.set(AddSourceMode::Markdown); selected.set(None); defer_account_focus("source-tab-markdown"); } else if event.key() == Key::ArrowRight && web_import_enabled { event.prevent_default(); mode.set(AddSourceMode::Web); selected.set(None); defer_account_focus("source-tab-web"); }, "PDF" }
                button { id: "source-tab-web", class: "secondary-action", r#type: "button", role: "tab", aria_selected: mode() == AddSourceMode::Web, aria_controls: "source-panel-web", aria_disabled: !web_import_enabled, disabled: !web_import_enabled, tabindex: if mode() == AddSourceMode::Web { "0" } else { "-1" }, onclick: move |_| { mode.set(AddSourceMode::Web); selected.set(None); }, onkeydown: move |event| if event.key() == Key::ArrowLeft && pdf_import_enabled { event.prevent_default(); mode.set(AddSourceMode::Pdf); defer_account_focus("source-tab-pdf"); } else if event.key() == Key::ArrowRight { event.prevent_default(); mode.set(AddSourceMode::Epub); defer_account_focus("source-tab-epub"); }, "Web-ссылка" }
            }
            if !capabilities_loaded {
                p { class: "capability-note", role: "status", "Проверяем поддержку импорта по URL…" }
            }
            if mode() == AddSourceMode::Epub {
                div { id: "source-panel-epub", role: "tabpanel", aria_labelledby: "source-tab-epub",
                p { "Книга EPUB без DRM, до 100 МБ. Lumi подготовит её для удобного чтения на любом экране." }
                label { class: "upload-dropzone",
                    span { class: "upload-icon", aria_hidden: "true", "＋" }
                    strong { if let Some(upload) = selected.read().as_ref() { "{upload.name}" } else { "Выберите файл EPUB" } }
                    small { if let Some(upload) = selected.read().as_ref() { "{upload.bytes.len()} байт" } else { ".epub · до 100 МБ" } }
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
                                    Ok(bytes) => selected.set(Some(SelectedUpload { name, bytes: bytes.to_vec() })),
                                    Err(_) => error.set("Не удалось прочитать выбранный EPUB.".to_owned()),
                                }
                            });
                        },
                    }
                }
                }
            } else if mode() == AddSourceMode::Lum {
                div { id: "source-panel-lum", role: "tabpanel", aria_labelledby: "source-tab-lum",
                    p { "Переносимая книга LUM до 100 МБ: главы, ссылки и локальные изображения в одном файле." }
                    label { class: "upload-dropzone",
                        span { class: "upload-icon", aria_hidden: "true", "＋" }
                        strong { if let Some(upload) = selected.read().as_ref() { "{upload.name}" } else { "Выберите файл LUM" } }
                        small { if let Some(upload) = selected.read().as_ref() { "{upload.bytes.len()} байт" } else { ".lum · до 100 МБ" } }
                        input {
                            r#type: "file",
                            name: "lum_file",
                            accept: ".lum,application/vnd.lumi.lum+zip",
                            disabled: busy(),
                            aria_label: "Файл LUM",
                            onchange: move |event| {
                                let Some(file) = event.files().into_iter().next() else { return; };
                                spawn(async move {
                                    let name = file.name();
                                    match file.read_bytes().await {
                                        Ok(bytes) if bytes.len() <= lumi_core::LUM_WEB_SOURCE_BYTES as usize => {
                                            error.set(String::new());
                                            selected.set(Some(SelectedUpload { name, bytes: bytes.to_vec() }));
                                        }
                                        Ok(_) => {
                                            selected.set(None);
                                            error.set("LUM превышает лимит 100 МБ.".to_owned());
                                        }
                                        Err(_) => error.set("Не удалось прочитать выбранный LUM.".to_owned()),
                                    }
                                });
                            },
                        }
                    }
                }
            } else if mode() == AddSourceMode::Markdown {
                div { id: "source-panel-markdown", role: "tabpanel", aria_labelledby: "source-tab-markdown",
                    p { "Документ Markdown до 10 МБ. Поддерживаются заголовки, ссылки, таблицы и списки задач." }
                    label { class: "upload-dropzone",
                        span { class: "upload-icon", aria_hidden: "true", "＋" }
                        strong { if let Some(upload) = selected.read().as_ref() { "{upload.name}" } else { "Выберите файл Markdown" } }
                        small { if let Some(upload) = selected.read().as_ref() { "{upload.bytes.len()} байт" } else { ".md, .markdown · до 10 МБ" } }
                        input {
                            r#type: "file",
                            name: "markdown_file",
                            accept: ".md,.markdown,text/markdown,text/x-markdown",
                            disabled: busy(),
                            aria_label: "Файл Markdown",
                            onchange: move |event| {
                                let Some(file) = event.files().into_iter().next() else { return; };
                                spawn(async move {
                                    let name = file.name();
                                    match file.read_bytes().await {
                                        Ok(bytes) if bytes.len() <= lumi_core::MARKDOWN_WEB_SOURCE_BYTES as usize => {
                                            error.set(String::new());
                                            selected.set(Some(SelectedUpload { name, bytes: bytes.to_vec() }));
                                        }
                                        Ok(_) => {
                                            selected.set(None);
                                            error.set("Markdown превышает лимит 10 МБ.".to_owned());
                                        }
                                        Err(_) => error.set("Не удалось прочитать выбранный Markdown.".to_owned()),
                                    }
                                });
                            },
                        }
                    }
                }
            } else if mode() == AddSourceMode::Pdf {
                div { id: "source-panel-pdf", role: "tabpanel", aria_labelledby: "source-tab-pdf",
                    p { "PDF до 200 МБ. Lumi сохранит исходный вид страниц и доступный текст." }
                    label { class: "upload-dropzone",
                        span { class: "upload-icon", aria_hidden: "true", "＋" }
                        strong { if let Some(upload) = selected.read().as_ref() { "{upload.name}" } else { "Выберите файл PDF" } }
                        small { if let Some(upload) = selected.read().as_ref() { "{upload.bytes.len()} байт" } else { ".pdf · до 200 МБ" } }
                        input {
                            r#type: "file",
                            name: "pdf_file",
                            accept: ".pdf,application/pdf",
                            disabled: busy(),
                            aria_label: "Файл PDF",
                            onchange: move |event| {
                                let Some(file) = event.files().into_iter().next() else { return; };
                                spawn(async move {
                                    let name = file.name();
                                    match file.read_bytes().await {
                                        Ok(bytes) => selected.set(Some(SelectedUpload { name, bytes: bytes.to_vec() })),
                                        Err(_) => error.set("Не удалось прочитать выбранный PDF.".to_owned()),
                                    }
                                });
                            },
                        }
                    }
                }
            } else {
                div { id: "source-panel-web", role: "tabpanel", aria_labelledby: "source-tab-web",
                p { "Вставьте публичную ссылку на статью. Lumi сохранит её содержание и выделит основной текст." }
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
                button { class: "primary-action", r#type: "button", disabled: busy() || (matches!(mode(), AddSourceMode::Epub | AddSourceMode::Lum | AddSourceMode::Markdown | AddSourceMode::Pdf) && selected.read().is_none()) || (mode() == AddSourceMode::Web && url().trim().is_empty()), onclick: move |_| {
                    let selected_upload = selected.read().clone();
                    let source_url = url();
                    let source_mode = mode();
                    let csrf = csrf_token.clone();
                    busy.set(true);
                    error.set(String::new());
                    spawn(async move {
                        let result = match source_mode {
                            AddSourceMode::Epub => match selected_upload.as_ref() {
                                Some(upload) => upload_document(&csrf, upload).await,
                                None => return,
                            },
                            AddSourceMode::Lum => match selected_upload.as_ref() {
                                Some(upload) => upload_document(&csrf, upload).await,
                                None => return,
                            },
                            AddSourceMode::Pdf => match selected_upload.as_ref() {
                                Some(upload) => upload_document(&csrf, upload).await,
                                None => return,
                            },
                            AddSourceMode::Markdown => match selected_upload.as_ref() {
                                Some(upload) => upload_document(&csrf, upload).await,
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
fn MaterialDetailsDialog(
    entry: LibraryEntry,
    csrf_token: String,
    abridgement_enabled: bool,
    on_close: EventHandler<()>,
) -> Element {
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
                if let Some(derivation) = entry.derivation.as_ref() {
                    div { dt { "Происхождение" } dd {
                        "Сокращение ревизии {derivation.source_revision_id}"
                        if derivation.source_changed { " · оригинал обновлён" }
                    } }
                }
            }
            if !entry.latest_job.diagnostics.is_empty() {
                section { class: "details-diagnostics", aria_label: "Диагностика импорта",
                    h3 { "Диагностика" }
                    for diagnostic in &entry.latest_job.diagnostics {
                        p { strong { "{diagnostic.code}" } " · {diagnostic.message}" }
                    }
                }
            }
            if let Some(derivation) = entry.derivation.as_ref() {
                nav { class: "summary-citations", aria_label: "Источники сокращённого материала",
                    for (index, citation) in derivation.source_refs.iter().take(8).enumerate() {
                        button {
                            class: "text-action",
                            r#type: "button",
                            onclick: {
                                let citation = citation.clone();
                                move |_| crate::ai::open_derived_source(&citation)
                            },
                            "Открыть источник {index + 1}"
                        }
                    }
                }
            }
            div { class: "dialog-actions",
                a { class: "secondary-action", href: "{API_BASE}/materials/{entry.id}/source", "{download_label}" }
                if let Some(revision_id) = entry.active_revision_id {
                    crate::ai::SummaryAction {
                        material_id: entry.id,
                        revision_id,
                        scope_kind: lumi_core::SummaryScopeKind::Material,
                        scope_ref: "material".to_owned(),
                        label: "Саммари материала".to_owned(),
                        csrf_token: csrf_token.clone(),
                    }
                    if abridgement_enabled && entry.derivation.is_none() {
                        crate::ai::AbridgementAction {
                            material_id: entry.id,
                            revision_id,
                            csrf_token: csrf_token.clone(),
                        }
                    }
                }
                if let Some(derivation) = entry.derivation.as_ref() {
                    a { class: "secondary-action", href: "#reader/{derivation.source_material_id}", "Открыть оригинал" }
                }
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
    let phrase_word_count = phrase().split_whitespace().count();
    let phrase_is_complete = phrase_word_count == 24;

    rsx! {
        main { id: "main-content", class: "account-screen", aria_label: "Lumi — регистрация и вход",
            section { class: "account-card",
                p { class: "eyebrow", "Защищённый аккаунт" }
                h1 { "Lumi" }
                p { "Фраза восстановления остаётся у вас. Сервер хранит только данные, необходимые для безопасного входа." }
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
                        }, "Создать фразу восстановления" }
                    } else {
                        div { class: "seed-phrase", aria_label: "Фраза восстановления", code { "{phrase}" } }
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
                        span { "Фраза восстановления (24 слова)" }
                        textarea { name: "recovery_phrase", rows: "5", value: "{phrase}", autocomplete: "off", spellcheck: "false", placeholder: "Введите 24 слова…", oninput: move |event| phrase.set(event.value()) }
                    }
                    p { class: if phrase().is_empty() || phrase_is_complete { "field-hint" } else { "field-hint field-hint-warning" }, aria_live: "polite",
                        if phrase().is_empty() {
                            "Введите слова через пробел."
                        } else {
                            "{phrase_word_count} из 24 слов"
                        }
                    }
                    button { class: "primary-action", r#type: "button", disabled: busy() || !phrase_is_complete, onclick: move |_| {
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

async fn load_mcp_connections() -> Result<AiPage<McpConnection>, ApiError> {
    let response = Request::get(&format!("{API_BASE}/mcp/connections"))
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(network_error)?;
    parse_json(response).await
}

async fn create_mcp_connection(
    csrf: &str,
    name: &str,
) -> Result<McpConnectionTokenResponse, ApiError> {
    let request = CreateMcpConnectionRequest {
        name: name.trim().to_owned(),
        idempotency_key: Uuid::now_v7().to_string(),
    };
    let request = Request::post(&format!("{API_BASE}/mcp/connections"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .json(&request)
        .map_err(network_error)?;
    parse_json(request.send().await.map_err(network_error)?).await
}

async fn rotate_mcp_connection(
    csrf: &str,
    connection: &McpConnection,
) -> Result<McpConnectionTokenResponse, ApiError> {
    let request = RevokeMcpConnectionRequest {
        expected_revision: connection.object_revision,
        idempotency_key: Uuid::now_v7().to_string(),
    };
    let request = Request::post(&format!(
        "{API_BASE}/mcp/connections/{}/rotate",
        connection.id
    ))
    .credentials(RequestCredentials::Include)
    .header("X-Lumi-CSRF", csrf)
    .json(&request)
    .map_err(network_error)?;
    parse_json(request.send().await.map_err(network_error)?).await
}

async fn revoke_mcp_connection(csrf: &str, connection: &McpConnection) -> Result<(), ApiError> {
    let request = RevokeMcpConnectionRequest {
        expected_revision: connection.object_revision,
        idempotency_key: Uuid::now_v7().to_string(),
    };
    let request = Request::delete(&format!("{API_BASE}/mcp/connections/{}", connection.id))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .json(&request)
        .map_err(network_error)?;
    let response = request.send().await.map_err(network_error)?;
    if response.ok() {
        Ok(())
    } else {
        Err(api_response_error(&response))
    }
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

async fn upload_document(csrf: &str, upload: &SelectedUpload) -> Result<AcceptedImport, ApiError> {
    let bytes = js_sys::Uint8Array::from(upload.bytes.as_slice());
    let parts = js_sys::Array::new();
    parts.push(&bytes);
    let blob = web_sys::Blob::new_with_u8_array_sequence(&parts)
        .map_err(|_| ApiError::Message("Не удалось подготовить документ к отправке.".to_owned()))?;
    let form = web_sys::FormData::new()
        .map_err(|_| ApiError::Message("Browser FormData недоступен.".to_owned()))?;
    form.append_with_blob_and_filename("file", &blob, &upload.name)
        .map_err(|_| ApiError::Message("Не удалось добавить документ в форму.".to_owned()))?;
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

fn reading_progress_label(progress_fraction: f32) -> String {
    let percent = (progress_fraction.clamp(0.0, 1.0) * 100.0).round() as u32;
    if progress_fraction > 0.0 && percent == 0 {
        "<1%".to_owned()
    } else {
        format!("{percent}%")
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
        lumi_core::JobStage::CapturingSnapshot => "Сохраняем копию страницы",
        lumi_core::JobStage::CapturingTelegramMedia => "Сохраняем фото из Telegram",
        lumi_core::JobStage::FetchingLinkedSources => "Загружаем связанные страницы",
        lumi_core::JobStage::ExtractingContent => "Извлекаем основной текст",
        lumi_core::JobStage::ValidatingContainer => "Проверяем контейнер",
        lumi_core::JobStage::InspectingDocument => "Проверяем PDF",
        lumi_core::JobStage::Normalizing => "Нормализуем главы",
        lumi_core::JobStage::Persisting => "Публикуем результат",
        lumi_core::JobStage::ReaderDocumentBuilt => "Готовим документ чтения",
        lumi_core::JobStage::Committed => "Импорт завершён",
    }
}

fn material_format_short(kind: &MaterialKind) -> &'static str {
    match kind {
        MaterialKind::Epub => "EPUB",
        MaterialKind::Pdf => "PDF",
        MaterialKind::WebPage => "WEB",
        MaterialKind::Telegram => "TG",
        MaterialKind::Markdown => "MD",
        MaterialKind::Lum => "LUM",
    }
}

fn material_format_label(kind: &MaterialKind) -> &'static str {
    match kind {
        MaterialKind::Epub => "EPUB · книга",
        MaterialKind::Pdf => "PDF · документ",
        MaterialKind::WebPage => "Web · статья",
        MaterialKind::Telegram => "Telegram · составной материал",
        MaterialKind::Markdown => "Markdown · документ",
        MaterialKind::Lum => "LUM · книга",
    }
}

fn material_source_download_label(kind: &MaterialKind) -> &'static str {
    match kind {
        MaterialKind::Epub => "Скачать исходник",
        MaterialKind::Pdf => "Скачать исходный PDF",
        MaterialKind::WebPage => "Скачать сохранённую страницу",
        MaterialKind::Telegram => "Скачать исходное Telegram-сообщение",
        MaterialKind::Markdown => "Скачать исходный Markdown",
        MaterialKind::Lum => "Скачать исходный LUM",
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
            "Фраза восстановления должна содержать ровно 24 слова.".to_owned(),
        ));
    }
    let entropy: [u8; 32] = mnemonic.to_entropy().try_into().map_err(|_| {
        ApiError::Message("Фраза восстановления должна кодировать 256 бит.".to_owned())
    })?;
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
