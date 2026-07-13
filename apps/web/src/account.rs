//! Minimal browser account flow for the Stage 1 persistent-account slice.

use bip39::{Language, Mnemonic};
use dioxus::prelude::*;
use gloo_net::http::Request;
use lumi_core::{
    decode_auth_bytes, encode_auth_bytes, AcceptedImport, AccountSummary, AuthChallenge,
    ChallengeResponse, CompleteLoginRequest, CreateChallengeRequest, DerivedAuthMaterial,
    ImportStatusEntry, ImportedFixture, Job, JobStatus, RegisterAccountRequest, SessionBootstrap,
};
use uuid::Uuid;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::RequestCredentials;

use super::S1Workspace;

const API_BASE: &str = "/api/v1";

#[derive(Clone)]
enum AccountState {
    Loading,
    SignedOut,
    SignedIn(AccountSummary),
    Failed(String),
}

#[component]
pub(crate) fn AccountGate(imported: ImportedFixture) -> Element {
    let mut state = use_signal(|| AccountState::Loading);
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
            section { class: "account-screen", aria_label: "Загрузка аккаунта",
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
                div { class: "account-session-bar", role: "region", aria_label: "Активная сессия",
                    span { "Аккаунт: {account_label}" }
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
                ImportPanel { csrf_token: csrf.read().clone() }
                S1Workspace { imported }
            }
        }
        AccountState::Failed(message) => rsx! {
            section { class: "account-screen", aria_label: "Ошибка аккаунта",
                p { class: "eyebrow", "Account unavailable" }
                h1 { "Не удалось подключиться к Lumi" }
                p { class: "reader-error", "{message}" }
                button { r#type: "button", onclick: move |_| state.set(AccountState::SignedOut), "Открыть вход" }
            }
        },
    }
}

#[derive(Clone)]
struct SelectedEpub {
    name: String,
    bytes: Vec<u8>,
}

#[component]
fn ImportPanel(csrf_token: String) -> Element {
    let mut selected = use_signal(|| Option::<SelectedEpub>::None);
    let mut entries = use_signal(Vec::<ImportStatusEntry>::new);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);

    use_effect(move || {
        spawn(async move {
            match load_imports().await {
                Ok(loaded) => entries.set(loaded),
                Err(load_error) => error.set(load_error.to_string()),
            }
        });
    });

    rsx! {
        section { class: "import-panel", aria_label: "Импорт EPUB",
            div {
                p { class: "eyebrow", "Real EPUB import · Stage 2" }
                h2 { "Загрузить EPUB" }
                p { "DRM-free reflowable EPUB до 100 MiB. Исходник и результат сохраняются между перезапусками." }
            }
            label { class: "import-file",
                span { "Файл EPUB" }
                input {
                    r#type: "file",
                    accept: ".epub,application/epub+zip",
                    disabled: busy(),
                    onchange: move |event| {
                        let Some(file) = event.files().into_iter().next() else {
                            return;
                        };
                        spawn(async move {
                            let name = file.name();
                            match file.read_bytes().await {
                                Ok(bytes) => selected.set(Some(SelectedEpub {
                                    name,
                                    bytes: bytes.to_vec(),
                                })),
                                Err(_) => error.set("Не удалось прочитать выбранный EPUB.".to_owned()),
                            }
                        });
                    },
                }
            }
            button {
                class: "primary-action",
                r#type: "button",
                disabled: busy() || selected.read().is_none(),
                onclick: move |_| {
                    let Some(upload) = selected.read().clone() else {
                        return;
                    };
                    let csrf = csrf_token.clone();
                    busy.set(true);
                    error.set(String::new());
                    spawn(async move {
                        match upload_epub(&csrf, &upload).await {
                            Ok(accepted) => {
                                selected.set(None);
                                match wait_for_job(accepted.job).await {
                                    Ok(_) => match load_imports().await {
                                        Ok(loaded) => entries.set(loaded),
                                        Err(load_error) => error.set(load_error.to_string()),
                                    },
                                    Err(job_error) => error.set(job_error.to_string()),
                                }
                            }
                            Err(upload_error) => error.set(upload_error.to_string()),
                        }
                        busy.set(false);
                    });
                },
                if busy() { "Импортируем…" } else { "Загрузить и импортировать" }
            }
            if let Some(upload) = selected.read().as_ref() {
                p { class: "import-selection", "Выбран: {upload.name} · {upload.bytes.len()} байт" }
            }
            if !error().is_empty() {
                p { class: "account-error", role: "alert", "{error}" }
            }
            div { class: "import-list", aria_live: "polite",
                for entry in entries.read().iter() {
                    ImportStatusCard { entry: entry.clone(), csrf_token: csrf_token.clone(), on_changed: move |_| {
                        spawn(async move {
                            if let Ok(loaded) = load_imports().await {
                                entries.set(loaded);
                            }
                        });
                    } }
                }
            }
        }
    }
}

#[component]
fn ImportStatusCard(
    entry: ImportStatusEntry,
    csrf_token: String,
    on_changed: EventHandler<()>,
) -> Element {
    let status_label = import_status_label(entry.job.status);
    let status_class = if matches!(entry.job.status, JobStatus::Succeeded) {
        "status-pill success"
    } else if matches!(entry.job.status, JobStatus::Failed | JobStatus::Cancelled) {
        "status-pill danger"
    } else {
        "status-pill"
    };
    let job_id = entry.job.id;
    let cancel_csrf = csrf_token.clone();
    let retry_csrf = csrf_token;
    let cancel_changed = on_changed;
    let retry_changed = on_changed;
    rsx! {
        article { class: "import-card", aria_label: "Импорт {entry.title}",
            header {
                strong { "{entry.title}" }
                span { class: status_class, "{status_label}" }
            }
            p { "Этап: {entry.job.stage:?}" }
            for diagnostic in entry.job.diagnostics.iter() {
                p { class: "import-diagnostic", role: "status",
                    strong { "{diagnostic.code}" }
                    span { " {diagnostic.message}" }
                }
            }
            div { class: "command-row" ,
                if matches!(entry.job.status, JobStatus::Queued | JobStatus::Running) {
                    button { class: "command-button", r#type: "button", onclick: move |_| {
                        let csrf = cancel_csrf.clone();
                        spawn(async move {
                            if let Ok(job) = mutate_job(job_id, "cancel", &csrf).await {
                                let _ = wait_for_job(job).await;
                                cancel_changed.call(());
                            }
                        });
                    }, "Отменить" }
                }
                if matches!(entry.job.status, JobStatus::Failed | JobStatus::Cancelled) {
                    button { class: "command-button", r#type: "button", onclick: move |_| {
                        let csrf = retry_csrf.clone();
                        spawn(async move {
                            if let Ok(job) = mutate_job(job_id, "retry", &csrf).await {
                                let _ = wait_for_job(job).await;
                                retry_changed.call(());
                            }
                        });
                    }, "Повторить" }
                }
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
                p { class: "eyebrow", "Persistent account · Stage 1" }
                h1 { "Lumi" }
                p { "Seed phrase остаётся в браузере. Сервер хранит только публичный ключ и отзывную сессию." }
                div { class: "account-tabs", role: "tablist", aria_label: "Действие с аккаунтом",
                    button {
                        r#type: "button",
                        role: "tab",
                        aria_selected: tab() == "register",
                        onclick: move |_| tab.set("register".to_owned()),
                        "Создать аккаунт"
                    }
                    button {
                        r#type: "button",
                        role: "tab",
                        aria_selected: tab() == "login",
                        onclick: move |_| tab.set("login".to_owned()),
                        "Войти / восстановить"
                    }
                }

                if tab() == "register" {
                    label { class: "account-field",
                        span { "Псевдоним (необязательно)" }
                        input {
                            value: "{nickname}",
                            maxlength: "80",
                            autocomplete: "nickname",
                            oninput: move |event| nickname.set(event.value()),
                        }
                    }
                    if phrase().is_empty() {
                        button {
                            class: "primary-action",
                            r#type: "button",
                            onclick: move |_| match Mnemonic::generate_in(Language::English, 24) {
                                Ok(mnemonic) => phrase.set(mnemonic.to_string()),
                                Err(generate_error) => error.set(generate_error.to_string()),
                            },
                            "Сгенерировать recovery phrase"
                        }
                    } else {
                        div { class: "seed-phrase", aria_label: "Recovery phrase",
                            code { "{phrase}" }
                        }
                        label { class: "account-confirm",
                            input {
                                r#type: "checkbox",
                                checked: confirmed(),
                                onchange: move |event| confirmed.set(event.checked()),
                            }
                            span { "Я сохранил(а) все 24 слова. Без них доступ нельзя восстановить." }
                        }
                        button {
                            class: "primary-action",
                            r#type: "button",
                            disabled: busy() || !confirmed(),
                            onclick: move |_| {
                                let seed_phrase = phrase.read().clone();
                                let display_name = nickname.read().clone();
                                busy.set(true);
                                error.set(String::new());
                                spawn(async move {
                                    match register(&seed_phrase, &display_name).await {
                                        Ok(session) => {
                                            phrase.set(String::new());
                                            on_authenticated.call(session);
                                        }
                                        Err(register_error) => error.set(register_error.to_string()),
                                    }
                                    busy.set(false);
                                });
                            },
                            if busy() { "Создаём…" } else { "Создать аккаунт" }
                        }
                    }
                } else {
                    label { class: "account-field",
                        span { "Recovery phrase (24 слова)" }
                        textarea {
                            rows: "5",
                            value: "{phrase}",
                            autocomplete: "off",
                            spellcheck: "false",
                            oninput: move |event| phrase.set(event.value()),
                        }
                    }
                    button {
                        class: "primary-action",
                        r#type: "button",
                        disabled: busy() || phrase().trim().is_empty(),
                        onclick: move |_| {
                            let seed_phrase = phrase.read().clone();
                            busy.set(true);
                            error.set(String::new());
                            spawn(async move {
                                match login(&seed_phrase).await {
                                    Ok(session) => {
                                        phrase.set(String::new());
                                        on_authenticated.call(session);
                                    }
                                    Err(login_error) => error.set(login_error.to_string()),
                                }
                                busy.set(false);
                            });
                        },
                        if busy() { "Проверяем…" } else { "Войти" }
                    }
                }
                if !error().is_empty() {
                    p { class: "account-error", role: "alert", "{error}" }
                }
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

async fn load_imports() -> Result<Vec<ImportStatusEntry>, ApiError> {
    let response = Request::get(&format!("{API_BASE}/imports"))
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(network_error)?;
    parse_json(response).await
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

fn import_status_label(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Queued => "В очереди",
        JobStatus::Running => "Импортируется",
        JobStatus::Succeeded => "Готово",
        JobStatus::Failed => "Ошибка",
        JobStatus::Cancelled => "Отменено",
    }
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
        return Err(if response.status() == 401 {
            ApiError::Unauthorized
        } else {
            ApiError::Message(format!("Lumi API вернул HTTP {}.", response.status()))
        });
    }
    response.json().await.map_err(network_error)
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
