//! Global Dioxus AI chat surface and Reader handoff adapter.

use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use gloo_net::http::Request;
use lumi_core::{
    AiContextAttachment, AiConversation, AiCredentialState, AiGeneration, AiGenerationStatus,
    AiMessageRole, AiMessageStatus, AiPage, AiProviderDescriptor, ConversationDetail,
    CreateConversationRequest, CreateMessageRequest, CreateMessageResponse,
    GenerationMutationRequest, ProviderCredentialState, PutProviderCredentialRequest,
    UpdateConversationRequest, ValidateProviderRequest,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use wasm_bindgen::{closure::Closure, JsCast};
use web_sys::RequestCredentials;

use super::account::{notify_session_expired, API_BASE};

const HANDOFF_STORAGE_KEY: &str = "lumi:ai-handoff:v1";
const READER_TARGET_STORAGE_KEY: &str = "lumi:reader-target:v1";
const HANDOFF_EVENT: &str = "lumi:ai-handoff";
pub(crate) const READER_TARGET_EVENT: &str = "lumi:reader-target";
const DEFAULT_MODEL: &str = "openai/gpt-4o-mini";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ReaderAiHandoff {
    pub(crate) attachment: AiContextAttachment,
    pub(crate) instruction: String,
    pub(crate) auto_submit: bool,
}

/// Persist one transient Reader handoff and notify the global shell.
pub(crate) fn dispatch_reader_handoff(handoff: &ReaderAiHandoff) -> Result<(), String> {
    let window = web_sys::window().ok_or_else(|| "Browser window недоступен.".to_owned())?;
    let storage = window
        .local_storage()
        .map_err(|_| "Browser storage недоступен.".to_owned())?
        .ok_or_else(|| "Browser storage отключён.".to_owned())?;
    let payload = serde_json::to_string(handoff)
        .map_err(|_| "Не удалось подготовить AI context.".to_owned())?;
    storage
        .set_item(HANDOFF_STORAGE_KEY, &payload)
        .map_err(|_| "Не удалось передать AI context.".to_owned())?;
    let event = web_sys::CustomEvent::new(HANDOFF_EVENT)
        .map_err(|_| "Не удалось открыть AI-чат.".to_owned())?;
    window
        .dispatch_event(&event)
        .map_err(|_| "Не удалось открыть AI-чат.".to_owned())?;
    Ok(())
}

#[component]
pub(crate) fn GlobalAiChat(csrf_token: String) -> Element {
    let mut open = use_signal(|| false);
    let mut settings_open = use_signal(|| false);
    let mut conversations = use_signal(Vec::<AiConversation>::new);
    let mut detail = use_signal(|| None::<ConversationDetail>);
    let mut draft = use_signal(String::new);
    let mut attachments = use_signal(Vec::<AiContextAttachment>::new);
    let mut provider = use_signal(|| None::<AiProviderDescriptor>);
    let mut provider_key = use_signal(String::new);
    let mut model = use_signal(|| DEFAULT_MODEL.to_owned());
    let mut title_draft = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(String::new);
    let mut last_generation = use_signal(|| None::<AiGeneration>);
    let csrf = use_signal(|| csrf_token);

    use_effect(move || {
        spawn(async move {
            match load_provider().await {
                Ok(value) => {
                    if let Some(first) = value.allowed_models.first() {
                        model.set(first.clone());
                    }
                    provider.set(Some(value));
                }
                Err(message) => error.set(message),
            }
            match load_conversations().await {
                Ok(items) => {
                    let first = items.first().map(|conversation| conversation.id);
                    conversations.set(items);
                    if let Some(id) = first {
                        if let Ok(value) = load_conversation(id).await {
                            title_draft.set(value.conversation.title.clone());
                            model.set(value.conversation.active_model.clone());
                            detail.set(Some(value));
                        }
                    }
                }
                Err(message) => error.set(message),
            }
        });
    });

    use_effect(move || {
        let Some(window) = web_sys::window() else {
            return;
        };
        let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            consume_reader_handoff(
                open,
                settings_open,
                conversations,
                detail,
                draft,
                attachments,
                model,
                busy,
                error,
                last_generation,
                csrf,
            );
        });
        let _ = window
            .add_event_listener_with_callback(HANDOFF_EVENT, handler.as_ref().unchecked_ref());
        handler.forget();
        consume_reader_handoff(
            open,
            settings_open,
            conversations,
            detail,
            draft,
            attachments,
            model,
            busy,
            error,
            last_generation,
            csrf,
        );
    });

    let provider_snapshot = provider.read().clone();
    let detail_snapshot = detail.read().clone();
    let conversation_items = conversations.read().clone();
    let attachment_items = attachments.read().clone();
    let configured = provider_snapshot
        .as_ref()
        .is_some_and(|value| value.credential_state == AiCredentialState::Valid);
    let active_generation = detail_snapshot
        .as_ref()
        .and_then(|value| value.active_generation.clone());

    rsx! {
        button {
            class: "ai-chat-trigger",
            r#type: "button",
            aria_expanded: open(),
            aria_controls: "global-ai-chat",
            onclick: move |_| {
                open.toggle();
                if open() { defer_focus("ai-chat-composer"); }
            },
            span { aria_hidden: "true", "✦" }
            "ИИ-чат"
        }
        if open() {
            aside {
                id: "global-ai-chat",
                class: "ai-chat-panel",
                aria_label: "Персональный AI-ассистент",
                onkeydown: move |event| if event.key() == Key::Escape { open.set(false); },
                header { class: "ai-chat-header",
                    div {
                        p { class: "eyebrow", "Персональный ассистент" }
                        h2 { "Lumi AI" }
                    }
                    div { class: "ai-chat-header-actions",
                        button { r#type: "button", aria_label: "Настройки OpenRouter", onclick: move |_| settings_open.toggle(), "⚙" }
                        button { r#type: "button", aria_label: "Свернуть AI-чат", onclick: move |_| open.set(false), "×" }
                    }
                }
                if !error().is_empty() {
                    p { class: "ai-chat-error", role: "alert",
                        "{error}"
                        button { r#type: "button", aria_label: "Закрыть ошибку", onclick: move |_| error.set(String::new()), "×" }
                    }
                }
                if settings_open() {
                    section { class: "ai-provider-settings", aria_label: "Настройки OpenRouter",
                        h3 { "OpenRouter BYOK" }
                        p { "В Lumi отправляются только явно прикреплённые фрагменты. Ключ хранится на сервере в зашифрованном виде и после сохранения не показывается." }
                        if let Some(current) = provider_snapshot.as_ref() {
                            p { role: "status",
                                "Статус: "
                                strong { "{credential_label(current.credential_state)}" }
                                if let Some(fingerprint) = &current.credential_fingerprint {
                                    span { " · {fingerprint}" }
                                }
                            }
                        }
                        label { "Модель",
                            input {
                                name: "openrouter_model",
                                value: "{model}",
                                oninput: move |event| model.set(event.value()),
                            }
                        }
                        label { "Ключ OpenRouter",
                            input {
                                r#type: "password",
                                name: "openrouter_key",
                                autocomplete: "off",
                                value: "{provider_key}",
                                oninput: move |event| provider_key.set(event.value()),
                            }
                        }
                        div { class: "dialog-actions",
                            button {
                                class: "primary-action",
                                r#type: "button",
                                disabled: busy() || provider_key().trim().is_empty() || model().trim().is_empty(),
                                onclick: move |_| {
                                    let key = provider_key.read().clone();
                                    let selected_model = model.read().clone();
                                    let csrf_token = csrf.read().clone();
                                    spawn(async move {
                                        busy.set(true);
                                        error.set(String::new());
                                        match save_provider_key(&key, &selected_model, &csrf_token).await {
                                            Ok(_) => {
                                                provider_key.set(String::new());
                                                match load_provider().await {
                                                    Ok(value) => provider.set(Some(value)),
                                                    Err(message) => error.set(message),
                                                }
                                            }
                                            Err(message) => error.set(message),
                                        }
                                        busy.set(false);
                                    });
                                },
                                "Проверить и сохранить"
                            }
                            if configured {
                                button {
                                    class: "secondary-action",
                                    r#type: "button",
                                    disabled: busy(),
                                    onclick: move |_| {
                                        let csrf_token = csrf.read().clone();
                                        spawn(async move {
                                            busy.set(true);
                                            match validate_saved_provider(&model(), &csrf_token).await {
                                                Ok(valid) if valid => {}
                                                Ok(_) => error.set("OpenRouter отклонил сохранённый ключ или модель.".to_owned()),
                                                Err(message) => error.set(message),
                                            }
                                            if let Ok(value) = load_provider().await {
                                                provider.set(Some(value));
                                            }
                                            busy.set(false);
                                        });
                                    },
                                    "Проверить ключ"
                                }
                                button {
                                    class: "danger-action",
                                    r#type: "button",
                                    disabled: busy(),
                                    onclick: move |_| {
                                        let csrf_token = csrf.read().clone();
                                        spawn(async move {
                                            busy.set(true);
                                            match delete_provider_key(&csrf_token).await {
                                                Ok(()) => {
                                                    if let Ok(value) = load_provider().await {
                                                        provider.set(Some(value));
                                                    }
                                                }
                                                Err(message) => error.set(message),
                                            }
                                            busy.set(false);
                                        });
                                    },
                                    "Удалить ключ"
                                }
                            }
                        }
                    }
                } else {
                    div { class: "ai-chat-body",
                        nav { class: "ai-conversation-list", aria_label: "Разговоры",
                            button {
                                class: "secondary-action",
                                r#type: "button",
                                disabled: busy(),
                                onclick: move |_| {
                                    let selected_model = model.read().clone();
                                    let csrf_token = csrf.read().clone();
                                    spawn(async move {
                                        busy.set(true);
                                        match create_conversation(&selected_model, &csrf_token).await {
                                            Ok(created) => {
                                                match load_conversation(created.id).await {
                                                    Ok(value) => {
                                                        title_draft.set(value.conversation.title.clone());
                                                        detail.set(Some(value));
                                                    }
                                                    Err(message) => error.set(message),
                                                }
                                                refresh_conversations(&mut conversations).await;
                                            }
                                            Err(message) => error.set(message),
                                        }
                                        busy.set(false);
                                    });
                                },
                                "+ Новый"
                            }
                            for conversation in conversation_items {
                                button {
                                    class: if detail_snapshot.as_ref().is_some_and(|current| current.conversation.id == conversation.id) { "active" } else { "" },
                                    r#type: "button",
                                    aria_current: if detail_snapshot.as_ref().is_some_and(|current| current.conversation.id == conversation.id) { "page" } else { "false" },
                                    onclick: move |_| {
                                        spawn(async move {
                                            match load_conversation(conversation.id).await {
                                                Ok(value) => {
                                                    title_draft.set(value.conversation.title.clone());
                                                    model.set(value.conversation.active_model.clone());
                                                    detail.set(Some(value));
                                                }
                                                Err(message) => error.set(message),
                                            }
                                        });
                                    },
                                    "{conversation.title}"
                                }
                            }
                        }
                        section { class: "ai-conversation", aria_label: "Текущий разговор",
                            if let Some(current) = detail_snapshot.clone() {
                                form { class: "ai-conversation-title", onsubmit: move |event| {
                                    event.prevent_default();
                                    let Some(conversation) = detail
                                        .read()
                                        .as_ref()
                                        .map(|value| value.conversation.clone())
                                    else {
                                        return;
                                    };
                                    let next_title = title_draft.read().clone();
                                    let csrf_token = csrf.read().clone();
                                    spawn(async move {
                                        match rename_conversation(&conversation, &next_title, &csrf_token).await {
                                            Ok(updated) => {
                                                if let Ok(value) = load_conversation(updated.id).await {
                                                    detail.set(Some(value));
                                                }
                                                refresh_conversations(&mut conversations).await;
                                            }
                                            Err(message) => error.set(message),
                                        }
                                    });
                                },
                                    label { class: "sr-only", r#for: "ai-conversation-title", "Название разговора" }
                                    input {
                                        id: "ai-conversation-title",
                                        value: "{title_draft}",
                                        oninput: move |event| title_draft.set(event.value()),
                                    }
                                    button { r#type: "submit", aria_label: "Сохранить название", "✓" }
                                    button {
                                        r#type: "button",
                                        class: "danger-link",
                                        aria_label: "Удалить разговор",
                                        disabled: current.active_generation.is_some(),
                                        onclick: move |_| {
                                            let id = current.conversation.id;
                                            let csrf_token = csrf.read().clone();
                                            spawn(async move {
                                                match delete_conversation(id, &csrf_token).await {
                                                    Ok(()) => {
                                                        detail.set(None);
                                                        refresh_conversations(&mut conversations).await;
                                                    }
                                                    Err(message) => error.set(message),
                                                }
                                            });
                                        },
                                        "Удалить"
                                    }
                                }
                                ol { class: "ai-message-list", aria_live: "polite",
                                    for message in current.messages.items.clone() {
                                        li { class: if message.role == AiMessageRole::User { "ai-message user" } else { "ai-message assistant" },
                                            p { class: "ai-message-role", if message.role == AiMessageRole::User { "Вы" } else { "Lumi AI" } }
                                            if message.content.is_empty() && message.status == AiMessageStatus::Streaming {
                                                p { role: "status", "Думает…" }
                                            } else {
                                                p { class: "ai-message-content", "{message.content}" }
                                            }
                                            if message.role == AiMessageRole::Assistant && !message.attachments.is_empty() {
                                                nav { class: "ai-citations", aria_label: "Источники ответа",
                                                    for (index, citation) in message.attachments.clone().into_iter().enumerate() {
                                                        button {
                                                            r#type: "button",
                                                            onclick: move |_| open_source(&citation),
                                                            "Источник {index + 1}: {citation.display_label}"
                                                        }
                                                    }
                                                }
                                            }
                                            if matches!(message.status, AiMessageStatus::Failed | AiMessageStatus::Cancelled) {
                                                span { class: "ai-message-state", "{message_status_label(message.status)}" }
                                            }
                                        }
                                    }
                                }
                                if !attachment_items.is_empty() {
                                    div { class: "ai-attachment-list", aria_label: "Прикреплённый контекст",
                                        for attachment in attachment_items.clone() {
                                            span {
                                                strong { "{attachment.display_label}" }
                                                button { r#type: "button", aria_label: "Убрать контекст", onclick: move |_| attachments.set(Vec::new()), "×" }
                                            }
                                        }
                                    }
                                }
                                form { class: "ai-composer", onsubmit: move |event| {
                                    event.prevent_default();
                                    let text = draft.read().clone();
                                    if text.trim().is_empty() { return; }
                                    let conversation = current.conversation.clone();
                                    let context = attachments.read().clone();
                                    let csrf_token = csrf.read().clone();
                                    spawn(async move {
                                        busy.set(true);
                                        error.set(String::new());
                                        match send_message_request(
                                            conversation.id,
                                            conversation.object_revision,
                                            text,
                                            context,
                                            &csrf_token,
                                        ).await {
                                            Ok(created) => {
                                                last_generation.set(Some(created.generation.clone()));
                                                draft.set(String::new());
                                                attachments.set(Vec::new());
                                                poll_conversation(
                                                    conversation.id,
                                                    &mut detail,
                                                    &mut conversations,
                                                    &mut last_generation,
                                                ).await;
                                            }
                                            Err(message) => error.set(message),
                                        }
                                        busy.set(false);
                                    });
                                },
                                    label { class: "sr-only", r#for: "ai-chat-composer", "Сообщение AI-ассистенту" }
                                    textarea {
                                        id: "ai-chat-composer",
                                        rows: "3",
                                        placeholder: if configured { "Спросите о материале или продолжите разговор…" } else { "Сначала добавьте ключ OpenRouter в настройках." },
                                        value: "{draft}",
                                        disabled: busy() || !configured,
                                        oninput: move |event| draft.set(event.value()),
                                        onkeydown: move |event| {
                                            if event.key() == Key::Enter && !event.modifiers().shift() {
                                                // Form submit remains available and accessible; Shift+Enter adds a line.
                                            }
                                        },
                                    }
                                    div { class: "ai-composer-actions",
                                        if let Some(active) = active_generation.clone() {
                                            button {
                                                class: "secondary-action",
                                                r#type: "button",
                                                onclick: move |_| {
                                                    let generation = active.clone();
                                                    let csrf_token = csrf.read().clone();
                                                    spawn(async move {
                                                        match mutate_generation(generation.id, "stop", &csrf_token).await {
                                                            Ok(value) => last_generation.set(Some(value)),
                                                            Err(message) => error.set(message),
                                                        }
                                                    });
                                                },
                                                "Остановить"
                                            }
                                        } else if let Some(last) = last_generation.read().clone() {
                                            if matches!(last.status, AiGenerationStatus::Failed | AiGenerationStatus::Cancelled) {
                                                button {
                                                    class: "secondary-action",
                                                    r#type: "button",
                                                    onclick: move |_| retry_generation_ui(last.clone(), "retry", csrf, detail, conversations, last_generation, error),
                                                    "Повторить"
                                                }
                                            } else if last.status == AiGenerationStatus::Completed {
                                                button {
                                                    class: "secondary-action",
                                                    r#type: "button",
                                                    onclick: move |_| retry_generation_ui(last.clone(), "regenerate", csrf, detail, conversations, last_generation, error),
                                                    "Ответить заново"
                                                }
                                            }
                                        }
                                        button {
                                            class: "primary-action",
                                            r#type: "submit",
                                            disabled: busy() || !configured || draft().trim().is_empty() || active_generation.is_some(),
                                            "Отправить"
                                        }
                                    }
                                }
                            } else {
                                div { class: "ai-empty",
                                    h3 { "Начните разговор" }
                                    p { "История, вложения и ответы сохраняются на сервере." }
                                    button {
                                        class: "primary-action",
                                        r#type: "button",
                                        onclick: move |_| {
                                            let selected_model = model.read().clone();
                                            let csrf_token = csrf.read().clone();
                                            spawn(async move {
                                                match create_conversation(&selected_model, &csrf_token).await {
                                                    Ok(created) => {
                                                        if let Ok(value) = load_conversation(created.id).await {
                                                            title_draft.set(value.conversation.title.clone());
                                                            detail.set(Some(value));
                                                        }
                                                        refresh_conversations(&mut conversations).await;
                                                    }
                                                    Err(message) => error.set(message),
                                                }
                                            });
                                        },
                                        "Новый разговор"
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

fn retry_generation_ui(
    generation: AiGeneration,
    action: &'static str,
    csrf: Signal<String>,
    mut detail: Signal<Option<ConversationDetail>>,
    mut conversations: Signal<Vec<AiConversation>>,
    mut last_generation: Signal<Option<AiGeneration>>,
    mut error: Signal<String>,
) {
    spawn_forever(async move {
        match mutate_generation(generation.id, action, &csrf()).await {
            Ok(created) => {
                last_generation.set(Some(created.clone()));
                poll_conversation(
                    created.conversation_id,
                    &mut detail,
                    &mut conversations,
                    &mut last_generation,
                )
                .await;
            }
            Err(message) => error.set(message),
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn consume_reader_handoff(
    mut open: Signal<bool>,
    mut settings_open: Signal<bool>,
    mut conversations: Signal<Vec<AiConversation>>,
    mut detail: Signal<Option<ConversationDetail>>,
    mut draft: Signal<String>,
    mut attachments: Signal<Vec<AiContextAttachment>>,
    model: Signal<String>,
    mut busy: Signal<bool>,
    mut error: Signal<String>,
    mut last_generation: Signal<Option<AiGeneration>>,
    csrf: Signal<String>,
) {
    let Some(handoff) = take_handoff() else {
        return;
    };
    open.set(true);
    settings_open.set(false);
    attachments.set(vec![handoff.attachment.clone()]);
    draft.set(handoff.instruction.clone());
    if !handoff.auto_submit {
        defer_focus("ai-chat-composer");
        return;
    }
    let csrf_token = csrf.read().clone();
    let selected_model = model.read().clone();
    spawn_forever(async move {
        busy.set(true);
        error.set(String::new());
        match ensure_conversation(
            &mut conversations,
            &mut detail,
            &selected_model,
            &csrf_token,
        )
        .await
        {
            Ok(conversation) => {
                match send_message_request(
                    conversation.id,
                    conversation.object_revision,
                    handoff.instruction,
                    vec![handoff.attachment],
                    &csrf_token,
                )
                .await
                {
                    Ok(created) => {
                        last_generation.set(Some(created.generation.clone()));
                        attachments.set(Vec::new());
                        draft.set(String::new());
                        poll_conversation(
                            created.generation.conversation_id,
                            &mut detail,
                            &mut conversations,
                            &mut last_generation,
                        )
                        .await;
                    }
                    Err(message) => error.set(message),
                }
            }
            Err(message) => error.set(message),
        }
        busy.set(false);
    });
}

async fn ensure_conversation(
    conversations: &mut Signal<Vec<AiConversation>>,
    detail: &mut Signal<Option<ConversationDetail>>,
    model: &str,
    csrf: &str,
) -> Result<AiConversation, String> {
    if let Some(current) = detail.read().as_ref() {
        return Ok(current.conversation.clone());
    }
    let created = create_conversation(model, csrf).await?;
    let loaded = load_conversation(created.id).await?;
    detail.set(Some(loaded));
    refresh_conversations(conversations).await;
    Ok(created)
}

async fn poll_conversation(
    conversation_id: Uuid,
    detail: &mut Signal<Option<ConversationDetail>>,
    conversations: &mut Signal<Vec<AiConversation>>,
    last_generation: &mut Signal<Option<AiGeneration>>,
) {
    for _ in 0..600 {
        match load_conversation(conversation_id).await {
            Ok(value) => {
                let active = value.active_generation.clone();
                detail.set(Some(value));
                if let Some(generation) = active {
                    last_generation.set(Some(generation));
                } else {
                    break;
                }
            }
            Err(_) => break,
        }
        browser_delay(250).await;
    }
    refresh_conversations(conversations).await;
}

async fn refresh_conversations(conversations: &mut Signal<Vec<AiConversation>>) {
    if let Ok(items) = load_conversations().await {
        conversations.set(items);
    }
}

fn take_handoff() -> Option<ReaderAiHandoff> {
    let storage = web_sys::window()?.local_storage().ok()??;
    let payload = storage.get_item(HANDOFF_STORAGE_KEY).ok()??;
    let _ = storage.remove_item(HANDOFF_STORAGE_KEY);
    serde_json::from_str(&payload).ok()
}

fn open_source(attachment: &AiContextAttachment) {
    let Some(window) = web_sys::window() else {
        return;
    };
    if let Ok(Some(storage)) = window.local_storage() {
        if let Ok(payload) = serde_json::to_string(attachment) {
            let _ = storage.set_item(READER_TARGET_STORAGE_KEY, &payload);
        }
    }
    let _ = window
        .location()
        .set_hash(&format!("reader/{}", attachment.material_id));
    if let Ok(event) = web_sys::CustomEvent::new(READER_TARGET_EVENT) {
        let _ = window.dispatch_event(&event);
    }
}

pub(crate) fn take_reader_target(material_id: Uuid) -> Option<AiContextAttachment> {
    let storage = web_sys::window()?.local_storage().ok()??;
    let payload = storage.get_item(READER_TARGET_STORAGE_KEY).ok()??;
    let attachment: AiContextAttachment = serde_json::from_str(&payload).ok()?;
    if attachment.material_id != material_id {
        return None;
    }
    let _ = storage.remove_item(READER_TARGET_STORAGE_KEY);
    Some(attachment)
}

async fn load_provider() -> Result<AiProviderDescriptor, String> {
    get_json("/providers/openrouter").await
}

async fn load_conversations() -> Result<Vec<AiConversation>, String> {
    get_json::<AiPage<AiConversation>>("/ai/conversations")
        .await
        .map(|page| page.items)
}

async fn create_conversation(model: &str, csrf: &str) -> Result<AiConversation, String> {
    post_json(
        "/ai/conversations",
        &CreateConversationRequest {
            title: "Новый разговор".to_owned(),
            active_model: model.to_owned(),
            idempotency_key: Uuid::now_v7().to_string(),
        },
        csrf,
    )
    .await
}

async fn load_conversation(id: Uuid) -> Result<ConversationDetail, String> {
    get_json(&format!("/ai/conversations/{id}")).await
}

async fn rename_conversation(
    conversation: &AiConversation,
    title: &str,
    csrf: &str,
) -> Result<AiConversation, String> {
    patch_json(
        &format!("/ai/conversations/{}", conversation.id),
        &UpdateConversationRequest {
            title: Some(title.trim().to_owned()),
            active_model: None,
            expected_revision: conversation.object_revision,
            idempotency_key: Uuid::now_v7().to_string(),
        },
        csrf,
    )
    .await
}

async fn delete_conversation(id: Uuid, csrf: &str) -> Result<(), String> {
    let response = Request::delete(&format!("{API_BASE}/ai/conversations/{id}"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .send()
        .await
        .map_err(network_error)?;
    response_ok(response).await.map(|_| ())
}

async fn send_message_request(
    conversation_id: Uuid,
    expected_revision: u64,
    content: String,
    attachments: Vec<AiContextAttachment>,
    csrf: &str,
) -> Result<CreateMessageResponse, String> {
    post_json(
        &format!("/ai/conversations/{conversation_id}/messages"),
        &CreateMessageRequest {
            content,
            attachments,
            expected_conversation_revision: expected_revision,
            idempotency_key: Uuid::now_v7().to_string(),
        },
        csrf,
    )
    .await
}

async fn mutate_generation(id: Uuid, action: &str, csrf: &str) -> Result<AiGeneration, String> {
    post_json(
        &format!("/ai/generations/{id}/{action}"),
        &GenerationMutationRequest {
            expected_revision: 1,
            idempotency_key: Uuid::now_v7().to_string(),
        },
        csrf,
    )
    .await
}

async fn save_provider_key(
    key: &str,
    model: &str,
    csrf: &str,
) -> Result<ProviderCredentialState, String> {
    put_json(
        "/providers/openrouter/credential",
        &PutProviderCredentialRequest {
            credential: key.to_owned(),
            validation_model: model.to_owned(),
            idempotency_key: Uuid::now_v7().to_string(),
        },
        csrf,
    )
    .await
}

async fn validate_saved_provider(model: &str, csrf: &str) -> Result<bool, String> {
    post_json::<_, lumi_core::ProviderValidationResult>(
        "/providers/openrouter/validate",
        &ValidateProviderRequest {
            model: model.to_owned(),
        },
        csrf,
    )
    .await
    .map(|result| result.valid)
}

async fn delete_provider_key(csrf: &str) -> Result<(), String> {
    let response = Request::delete(&format!("{API_BASE}/providers/openrouter/credential"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .send()
        .await
        .map_err(network_error)?;
    response_ok(response).await.map(|_| ())
}

async fn get_json<T: for<'de> Deserialize<'de>>(path: &str) -> Result<T, String> {
    let response = Request::get(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(network_error)?;
    parse_json(response).await
}

async fn post_json<T: Serialize, R: for<'de> Deserialize<'de>>(
    path: &str,
    value: &T,
    csrf: &str,
) -> Result<R, String> {
    let mut request =
        Request::post(&format!("{API_BASE}{path}")).credentials(RequestCredentials::Include);
    if !csrf.is_empty() {
        request = request.header("X-Lumi-CSRF", csrf);
    }
    let response = request
        .json(value)
        .map_err(network_error)?
        .send()
        .await
        .map_err(network_error)?;
    parse_json(response).await
}

async fn put_json<T: Serialize, R: for<'de> Deserialize<'de>>(
    path: &str,
    value: &T,
    csrf: &str,
) -> Result<R, String> {
    let response = Request::put(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .json(value)
        .map_err(network_error)?
        .send()
        .await
        .map_err(network_error)?;
    parse_json(response).await
}

async fn patch_json<T: Serialize, R: for<'de> Deserialize<'de>>(
    path: &str,
    value: &T,
    csrf: &str,
) -> Result<R, String> {
    let response = Request::patch(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .json(value)
        .map_err(network_error)?
        .send()
        .await
        .map_err(network_error)?;
    parse_json(response).await
}

async fn parse_json<T: for<'de> Deserialize<'de>>(
    response: gloo_net::http::Response,
) -> Result<T, String> {
    if !response.ok() {
        return Err(response_error(&response).await);
    }
    response.json().await.map_err(network_error)
}

async fn response_ok(response: gloo_net::http::Response) -> Result<(), String> {
    if response.ok() {
        Ok(())
    } else {
        Err(response_error(&response).await)
    }
}

async fn response_error(response: &gloo_net::http::Response) -> String {
    if response.status() == 401 {
        notify_session_expired();
    }
    format!("Lumi API вернул HTTP {}.", response.status())
}

fn network_error(error: impl std::fmt::Display) -> String {
    format!("Сеть/API недоступны: {error}")
}

fn credential_label(state: AiCredentialState) -> &'static str {
    match state {
        AiCredentialState::Missing => "ключ не добавлен",
        AiCredentialState::Unvalidated => "не проверен",
        AiCredentialState::Valid => "готов",
        AiCredentialState::Invalid => "нужна повторная проверка",
    }
}

fn message_status_label(status: AiMessageStatus) -> &'static str {
    match status {
        AiMessageStatus::Failed => "Ответ завершился ошибкой",
        AiMessageStatus::Cancelled => "Ответ остановлен",
        AiMessageStatus::Committed | AiMessageStatus::Streaming | AiMessageStatus::Completed => "",
    }
}

fn defer_focus(id: &'static str) {
    spawn(async move {
        browser_delay(30).await;
        let Some(element) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id(id))
        else {
            return;
        };
        if let Ok(element) = element.dyn_into::<web_sys::HtmlElement>() {
            let _ = element.focus();
        }
    });
}

async fn browser_delay(milliseconds: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            let _ = window
                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, milliseconds);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
