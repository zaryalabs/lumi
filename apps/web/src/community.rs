//! Capability-aware Community Space Web surface.

use dioxus::prelude::*;
use gloo_net::http::Request;
use lumi_core::{
    ClaimSharedMaterialRequest, CommunityAccessLink, CommunityAccessLinkStatus,
    CommunityActivityEvent, CommunityActivityKind, CommunityActivityPage, CommunityLinkPreview,
    CommunityMembership, CommunityMembershipStatus, CommunityRole, CommunitySpace,
    CommunitySpaceDetail, CreateCommunityAccessLinkRequest, CreateCommunitySpaceRequest,
    CreateSharedChatMessageRequest, CreateSharedCommentRequest, CreateSharedThreadRequest,
    CreatedCommunityAccessLink, DeleteSharedChatMessageRequest, DeleteSharedCommentRequest,
    JoinCommunityLinkRequest, LibraryEntry, MaterialImportStatus, ModerateSocialContentRequest,
    ModerationAction, ModerationActionKind, ModerationTargetType, PreviewCommunityLinkRequest,
    ShareMaterialRequest, SharedChatMessage, SharedChatPage, SharedComment, SharedCommentThread,
    SharedDiscussionPage, SharedMaterial, SocialContentState, UpdateCommunityMemberRequest,
    UpdateCommunitySpaceRequest, UpdateSharedChatMessageRequest, UpdateSharedCommentRequest,
    UserMaterialClaimStatus,
};
use uuid::Uuid;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{closure::Closure, JsCast};
use web_sys::RequestCredentials;

use crate::account::{api_response_error, network_error, parse_json, ApiError, API_BASE};

#[component]
pub(crate) fn CommunityPage(
    current_user_id: Uuid,
    csrf_token: String,
    space_id: Option<Uuid>,
    join_token: Option<String>,
    available: bool,
    material_sharing_available: bool,
    material_discussions_available: bool,
    community_communications_available: bool,
    community_images_available: bool,
    on_open_space: EventHandler<Uuid>,
    on_open_list: EventHandler<()>,
) -> Element {
    if !available {
        return rsx! {
            main { id: "main-content", class: "library-view community-view", aria_label: "Сообщества Lumi",
                section { class: "library-error-state", role: "status",
                    p { class: "eyebrow", "Community" }
                    h1 { "Сообщества пока недоступны" }
                    p { "Этот сервер не публикует capability community-spaces." }
                }
            }
        };
    }
    if let Some(token) = join_token {
        return rsx! {
            JoinCommunity {
                token,
                csrf_token,
                on_joined: move |space_id| on_open_space.call(space_id),
                on_cancel: move |_| on_open_list.call(()),
            }
        };
    }
    if let Some(space_id) = space_id {
        return rsx! {
            CommunityDetail {
                current_user_id,
                csrf_token,
                space_id,
                material_sharing_available,
                material_discussions_available,
                community_communications_available,
                community_images_available,
                on_close: move |_| on_open_list.call(()),
            }
        };
    }
    rsx! {
        CommunityList {
            csrf_token,
            on_open_space: move |space_id| on_open_space.call(space_id),
        }
    }
}

#[component]
fn CommunityList(csrf_token: String, on_open_space: EventHandler<Uuid>) -> Element {
    let mut spaces = use_signal(|| Option::<Vec<CommunitySpace>>::None);
    let mut error = use_signal(String::new);
    let mut name = use_signal(String::new);
    let mut description = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut generation = use_signal(|| 0_u64);
    use_effect(move || {
        let _ = generation();
        spawn(async move {
            match load_spaces().await {
                Ok(value) => {
                    spaces.set(Some(value));
                    error.set(String::new());
                }
                Err(load_error) => error.set(load_error.to_string()),
            }
        });
    });
    let loaded = spaces.read().clone();
    rsx! {
        main { id: "main-content", class: "library-view community-view", aria_label: "Сообщества Lumi",
            header { class: "library-hero compact",
                div {
                    p { class: "eyebrow", "Community" }
                    h1 { "Сообщества" }
                    p { class: "library-lead", "Закрытые пространства для совместного чтения и обсуждения." }
                }
            }
            if !error().is_empty() {
                div { class: "library-alert", role: "alert",
                    p { "{error}" }
                    button { r#type: "button", onclick: move |_| generation += 1, "Повторить" }
                }
            }
            section { class: "library-section community-create", aria_label: "Создание сообщества",
                h2 { "Создать пространство" }
                label { "Название"
                    input {
                        value: "{name}",
                        maxlength: "120",
                        oninput: move |event| name.set(event.value()),
                    }
                }
                label { "Описание"
                    textarea {
                        value: "{description}",
                        maxlength: "4096",
                        oninput: move |event| description.set(event.value()),
                    }
                }
                button {
                    class: "primary-action",
                    r#type: "button",
                    disabled: busy() || name().trim().is_empty(),
                    onclick: move |_| {
                        let csrf = csrf_token.clone();
                        let request = CreateCommunitySpaceRequest {
                            name: name(),
                            description: Some(description()),
                        };
                        busy.set(true);
                        error.set(String::new());
                        spawn(async move {
                            match create_space(&csrf, &request).await {
                                Ok(detail) => on_open_space.call(detail.space.id),
                                Err(create_error) => error.set(create_error.to_string()),
                            }
                            busy.set(false);
                        });
                    },
                    if busy() { "Создаём…" } else { "Создать пространство" }
                }
            }
            section { class: "library-section community-memberships", aria_label: "Ваши сообщества",
                h2 { "Ваши сообщества" }
                match loaded {
                    None => rsx! { p { role: "status", "Загружаем пространства…" } },
                    Some(values) if values.is_empty() => rsx! {
                        div { class: "library-empty compact",
                            h3 { "Пока нет сообществ" }
                            p { "Создайте пространство или откройте полученную ссылку-приглашение." }
                        }
                    },
                    Some(values) => rsx! {
                        div { class: "community-grid",
                            for space in values {
                                article { class: "community-card",
                                    p { class: "eyebrow", "{space.member_count} участников" }
                                    h3 { "{space.name}" }
                                    if let Some(description) = &space.description {
                                        p { "{description}" }
                                    }
                                    button {
                                        r#type: "button",
                                        onclick: move |_| on_open_space.call(space.id),
                                        "Открыть"
                                    }
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

#[component]
fn JoinCommunity(
    token: String,
    csrf_token: String,
    on_joined: EventHandler<Uuid>,
    on_cancel: EventHandler<()>,
) -> Element {
    let mut preview = use_signal(|| Option::<CommunityLinkPreview>::None);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let token_for_load = token.clone();
    use_effect(move || {
        let token = token_for_load.clone();
        spawn(async move {
            match preview_link(&token).await {
                Ok(value) => preview.set(Some(value)),
                Err(load_error) => error.set(load_error.to_string()),
            }
        });
    });
    let preview_value = preview.read().clone();
    rsx! {
        main { id: "main-content", class: "library-view community-view", aria_label: "Вступление в сообщество",
            section { class: "account-card community-preview",
                p { class: "eyebrow", "Приглашение в Community Space" }
                if let Some(value) = preview_value {
                    h1 { "{value.name}" }
                    if let Some(description) = value.description {
                        p { "{description}" }
                    }
                    p { "{value.member_count} участников" }
                    p { class: "privacy-note", "Вступление не открывает чужие файлы, личные заметки, прогресс или обучение." }
                    div { class: "dialog-actions",
                        button {
                            class: "primary-action",
                            r#type: "button",
                            disabled: busy(),
                            onclick: move |_| {
                                let csrf = csrf_token.clone();
                                let token = token.clone();
                                busy.set(true);
                                error.set(String::new());
                                spawn(async move {
                                    match join_space(&csrf, &token).await {
                                        Ok(detail) => on_joined.call(detail.space.id),
                                        Err(join_error) => error.set(join_error.to_string()),
                                    }
                                    busy.set(false);
                                });
                            },
                            if busy() { "Вступаем…" } else { "Вступить" }
                        }
                        button { class: "secondary-action", r#type: "button", onclick: move |_| on_cancel.call(()), "Отмена" }
                    }
                } else if error().is_empty() {
                    p { role: "status", "Проверяем приглашение…" }
                }
                if !error().is_empty() {
                    div { class: "library-alert", role: "alert",
                        p { "{error}" }
                        button { r#type: "button", onclick: move |_| on_cancel.call(()), "К списку сообществ" }
                    }
                }
            }
        }
    }
}

#[component]
fn CommunityDetail(
    current_user_id: Uuid,
    csrf_token: String,
    space_id: Uuid,
    material_sharing_available: bool,
    material_discussions_available: bool,
    community_communications_available: bool,
    community_images_available: bool,
    on_close: EventHandler<()>,
) -> Element {
    let csrf_token = use_signal(|| csrf_token);
    let mut detail = use_signal(|| Option::<CommunitySpaceDetail>::None);
    let mut members = use_signal(Vec::<CommunityMembership>::new);
    let mut links = use_signal(Vec::<CommunityAccessLink>::new);
    let mut materials = use_signal(|| Option::<Vec<SharedMaterial>>::None);
    let mut generated_link = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut generation = use_signal(|| 0_u64);
    use_effect(move || {
        let _ = generation();
        spawn(async move {
            match load_space_bundle(space_id).await {
                Ok((loaded_detail, loaded_members, loaded_links)) => {
                    detail.set(Some(loaded_detail));
                    members.set(loaded_members);
                    links.set(loaded_links);
                    error.set(String::new());
                    if material_sharing_available {
                        match load_shared_materials(space_id).await {
                            Ok(loaded_materials) => materials.set(Some(loaded_materials)),
                            Err(load_error) => error.set(load_error.to_string()),
                        }
                    }
                }
                Err(load_error) => error.set(load_error.to_string()),
            }
        });
    });
    let detail_value = detail.read().clone();
    rsx! {
        main { id: "main-content", class: "library-view community-view", aria_label: "Пространство сообщества",
            button { class: "secondary-action compact-action", r#type: "button", onclick: move |_| on_close.call(()), "← Все сообщества" }
            if !error().is_empty() {
                div { class: "library-alert", role: "alert",
                    p { "{error}" }
                    button { r#type: "button", onclick: move |_| generation += 1, "Повторить" }
                }
            }
            if let Some(current) = detail_value {
                header { class: "library-hero compact community-header",
                    if community_images_available && current.space.cover.is_some() {
                        img {
                            class: "community-cover",
                            src: "{API_BASE}/spaces/{space_id}/images/cover",
                            alt: "",
                        }
                    }
                    if community_images_available && current.space.avatar.is_some() {
                        img {
                            class: "community-avatar",
                            src: "{API_BASE}/spaces/{space_id}/images/avatar",
                            alt: "Аватар сообщества {current.space.name}",
                        }
                    }
                    div {
                        p { class: "eyebrow", "{role_label(current.membership.role)} · {current.space.member_count} участников" }
                        h1 { "{current.space.name}" }
                        if let Some(description) = &current.space.description { p { class: "library-lead", "{description}" } }
                    }
                }
                if current.permissions.can_edit_space {
                    SpaceIdentityEditor {
                        csrf_token: csrf_token(),
                        detail: current.clone(),
                        community_images_available,
                        on_saved: move |_| generation += 1,
                    }
                }
                if current.permissions.can_manage_links {
                    section { class: "library-section community-links", aria_label: "Ссылки доступа",
                        h2 { "Ссылка для вступления" }
                        p { "Ссылка открывает безопасный preview. Membership создаётся только после кнопки «Вступить»." }
                        button {
                            class: "primary-action",
                            r#type: "button",
                            disabled: busy(),
                            onclick: move |_| {
                                let csrf = csrf_token();
                                busy.set(true);
                                spawn(async move {
                                    match create_access_link(&csrf, space_id).await {
                                        Ok(created) => {
                                            generated_link.set(join_url(&created.token));
                                            generation += 1;
                                        }
                                        Err(link_error) => error.set(link_error.to_string()),
                                    }
                                    busy.set(false);
                                });
                            },
                            "Создать новую ссылку"
                        }
                        if !generated_link().is_empty() {
                            label { "Скопируйте ссылку — после закрытия она больше не показывается"
                                input { readonly: true, value: "{generated_link}" }
                            }
                            button { r#type: "button", onclick: move |_| copy_to_clipboard(&generated_link()), "Копировать ссылку" }
                        }
                        ul { class: "community-link-list",
                            for link in links.read().iter() {
                                li {
                                    span { "{link_status_label(link.status)} · использований: {link.use_count}" }
                                    if link.status == CommunityAccessLinkStatus::Active {
                                        button {
                                            r#type: "button",
                                            onclick: {
                                                let link_id = link.id;
                                                let csrf = csrf_token();
                                                move |_| {
                                                    let csrf = csrf.clone();
                                                    spawn(async move {
                                                        match rotate_access_link(&csrf, space_id, link_id).await {
                                                            Ok(created) => {
                                                                generated_link.set(join_url(&created.token));
                                                                generation += 1;
                                                            }
                                                            Err(link_error) => error.set(link_error.to_string()),
                                                        }
                                                    });
                                                }
                                            },
                                            "Перевыпустить"
                                        }
                                        button {
                                            r#type: "button",
                                            onclick: {
                                                let link_id = link.id;
                                                let csrf = csrf_token();
                                                move |_| {
                                                    let csrf = csrf.clone();
                                                    spawn(async move {
                                                        match revoke_access_link(&csrf, space_id, link_id).await {
                                                            Ok(()) => generation += 1,
                                                            Err(link_error) => error.set(link_error.to_string()),
                                                        }
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
                section { class: "library-section", aria_label: "Участники сообщества",
                    h2 { "Участники" }
                    ul { class: "community-member-list",
                        for member in members.read().iter() {
                            li {
                                div {
                                    strong { "{member.nickname.as_deref().unwrap_or(\"Без псевдонима\")}" }
                                    span { " · {role_label(member.role)}" }
                                }
                                if current.permissions.can_assign_admin
                                    && member.user_id != current_user_id
                                    && member.status == CommunityMembershipStatus::Active
                                    && member.role != CommunityRole::Owner
                                {
                                    button {
                                        r#type: "button",
                                        onclick: {
                                            let member = member.clone();
                                            let csrf = csrf_token();
                                            move |_| {
                                                let csrf = csrf.clone();
                                                let next_role = if member.role == CommunityRole::Admin { CommunityRole::Member } else { CommunityRole::Admin };
                                                let member = member.clone();
                                                spawn(async move {
                                                    match change_member_role(&csrf, space_id, &member, next_role).await {
                                                        Ok(_) => generation += 1,
                                                        Err(member_error) => error.set(member_error.to_string()),
                                                    }
                                                });
                                            }
                                        },
                                        if member.role == CommunityRole::Admin { "Сделать участником" } else { "Назначить администратором" }
                                    }
                                }
                                if current.permissions.can_remove_members
                                    && member.user_id != current_user_id
                                    && member.role == CommunityRole::Member
                                    && member.status == CommunityMembershipStatus::Active
                                {
                                    button {
                                        r#type: "button",
                                        onclick: {
                                            let user_id = member.user_id;
                                            let csrf = csrf_token();
                                            move |_| {
                                                let csrf = csrf.clone();
                                                spawn(async move {
                                                    match remove_member(&csrf, space_id, user_id).await {
                                                        Ok(()) => generation += 1,
                                                        Err(member_error) => error.set(member_error.to_string()),
                                                    }
                                                });
                                            }
                                        },
                                        "Удалить"
                                    }
                                }
                            }
                        }
                    }
                }
                section { class: "community-shell-grid", aria_label: "Разделы сообщества",
                    article { class: "library-section community-materials", aria_label: "Материалы сообщества",
                        h2 { "Материалы" }
                        if !material_sharing_available {
                            p { "Этот сервер не публикует capability material-sharing." }
                        } else {
                            match materials.read().clone() {
                                None => rsx! { p { role: "status", "Загружаем материалы…" } },
                                Some(values) if values.is_empty() => rsx! {
                                    p { "Пока никто не добавил материал. Используйте «Поделиться» в библиотеке или Reader." }
                                },
                                Some(values) => rsx! {
                                    div { class: "community-material-list",
                                        for material in values {
                                            CommunityMaterialCard {
                                                material,
                                                space_id,
                                                csrf_token: csrf_token(),
                                                current_user_id,
                                                can_moderate: matches!(current.membership.role, CommunityRole::Owner | CommunityRole::Admin),
                                                discussions_available: material_discussions_available,
                                                on_changed: move |_| generation += 1,
                                            }
                                        }
                                    }
                                },
                            }
                        }
                    }
                    article { class: "library-section",
                        h2 { "Обсуждение и чат" }
                        if material_discussions_available {
                            p { "Обсуждения материалов доступны в их карточках. Комментарии по фрагментам и опубликованные выделения доступны в Reader после подключения своей копии." }
                        } else {
                            p { "Обсуждения материалов пока не включены capability-флагом." }
                        }
                        if community_communications_available {
                            SpaceCommunications {
                                space_id,
                                csrf_token: csrf_token(),
                                current_user_id,
                                can_moderate: matches!(current.membership.role, CommunityRole::Owner | CommunityRole::Admin),
                            }
                        } else {
                            p { "Чат и лента активности не включены capability-флагом." }
                        }
                    }
                }
                if current.membership.role != CommunityRole::Owner {
                    button {
                        class: "secondary-action danger-action",
                        r#type: "button",
                        onclick: move |_| {
                            let csrf = csrf_token();
                            spawn(async move {
                                match leave_space(&csrf, space_id).await {
                                    Ok(()) => on_close.call(()),
                                    Err(leave_error) => error.set(leave_error.to_string()),
                                }
                            });
                        },
                        "Выйти из сообщества"
                    }
                }
            } else if error().is_empty() {
                p { role: "status", "Загружаем пространство…" }
            }
        }
    }
}

#[component]
fn SpaceCommunications(
    space_id: Uuid,
    csrf_token: String,
    current_user_id: Uuid,
    can_moderate: bool,
) -> Element {
    let mut chat = use_signal(|| Option::<SharedChatPage>::None);
    let mut activity = use_signal(|| Option::<CommunityActivityPage>::None);
    let mut body = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut generation = use_signal(|| 0_u64);
    let mut poll_ticket = use_signal(|| 0_u64);
    use_effect(move || {
        let _ = generation();
        let next_ticket = poll_ticket.peek().saturating_add(1);
        poll_ticket.set(next_ticket);
        spawn(async move {
            let chat_result = load_space_chat(space_id, None).await;
            let activity_result = load_space_activity(space_id, None).await;
            match (chat_result, activity_result) {
                (Ok(messages), Ok(events)) => {
                    let current_chat = chat.peek().clone();
                    let current_activity = activity.peek().clone();
                    chat.set(Some(merge_chat_pages(current_chat, messages, true)));
                    activity.set(Some(merge_activity_pages(current_activity, events, true)));
                    error.set(String::new());
                    schedule_visible_refresh(generation, poll_ticket, 5_000);
                }
                (Err(load_error), _) | (_, Err(load_error)) => {
                    error.set(load_error.to_string());
                    schedule_visible_refresh(generation, poll_ticket, 15_000);
                }
            }
        });
    });
    let chat_next_cursor = chat
        .read()
        .as_ref()
        .and_then(|page| page.next_cursor.clone());
    let activity_next_cursor = activity
        .read()
        .as_ref()
        .and_then(|page| page.next_cursor.clone());
    rsx! {
        section { class: "community-communications", aria_label: "Чат и активность сообщества",
            if !error().is_empty() {
                div { class: "library-alert", role: "alert",
                    p { "{error}" }
                    button { r#type: "button", onclick: move |_| generation += 1, "Повторить" }
                }
            }
            section { class: "community-chat", aria_label: "Чат сообщества",
                h3 { "Чат" }
                form {
                    class: "community-comment-form compact",
                    onsubmit: move |event| {
                        event.prevent_default();
                        if body().trim().is_empty() || busy() {
                            return;
                        }
                        let csrf = csrf_token.clone();
                        let message_body = body();
                        busy.set(true);
                        spawn(async move {
                            match create_space_chat_message(&csrf, space_id, &message_body).await {
                                Ok(_) => {
                                    body.set(String::new());
                                    generation += 1;
                                }
                                Err(send_error) => error.set(send_error.to_string()),
                            }
                            busy.set(false);
                        });
                    },
                    label { "Новое сообщение"
                        textarea {
                            value: "{body}",
                            maxlength: "16384",
                            oninput: move |event| body.set(event.value()),
                        }
                    }
                    button { r#type: "submit", disabled: busy() || body().trim().is_empty(), "Отправить" }
                }
                match chat.read().clone() {
                    None => rsx! { p { role: "status", "Загружаем чат…" } },
                    Some(page) if page.messages.is_empty() => rsx! { p { "Сообщений пока нет." } },
                    Some(page) => rsx! {
                        ul { class: "community-chat-list",
                            for message in page.messages {
                                li {
                                    SpaceChatMessageView {
                                        message,
                                        space_id,
                                        csrf_token: csrf_token.clone(),
                                        current_user_id,
                                        can_moderate,
                                        on_changed: move |_| generation += 1,
                                    }
                                }
                            }
                        }
                    },
                }
                if let Some(cursor) = chat_next_cursor {
                    button {
                        class: "secondary-action compact-action",
                        r#type: "button",
                        disabled: busy(),
                        onclick: move |_| {
                            busy.set(true);
                            let cursor = cursor.clone();
                            spawn(async move {
                                match load_space_chat(space_id, Some(&cursor)).await {
                                    Ok(next_page) => {
                                        let current = chat.peek().clone();
                                        chat.set(Some(merge_chat_pages(current, next_page, false)));
                                    }
                                    Err(load_error) => error.set(load_error.to_string()),
                                }
                                busy.set(false);
                            });
                        },
                        "Загрузить ещё сообщения"
                    }
                }
            }
            section { class: "community-activity", aria_label: "Активность сообщества",
                h3 { "Активность" }
                match activity.read().clone() {
                    None => rsx! { p { role: "status", "Загружаем активность…" } },
                    Some(page) if page.events.is_empty() => rsx! { p { "Событий пока нет." } },
                    Some(page) => rsx! {
                        ol { class: "community-activity-list",
                            for event in page.events {
                                li {
                                    strong { "{event.actor_nickname.as_deref().unwrap_or(\"Система\")}" }
                                    span { " {activity_label(&event)}" }
                                }
                            }
                        }
                    },
                }
                if let Some(cursor) = activity_next_cursor {
                    button {
                        class: "secondary-action compact-action",
                        r#type: "button",
                        onclick: move |_| {
                            let cursor = cursor.clone();
                            spawn(async move {
                                match load_space_activity(space_id, Some(&cursor)).await {
                                    Ok(next_page) => {
                                        let current = activity.peek().clone();
                                        activity.set(Some(merge_activity_pages(
                                            current, next_page, false,
                                        )));
                                    }
                                    Err(load_error) => error.set(load_error.to_string()),
                                }
                            });
                        },
                        "Загрузить ещё события"
                    }
                }
            }
        }
    }
}

#[component]
fn SpaceChatMessageView(
    message: SharedChatMessage,
    space_id: Uuid,
    csrf_token: String,
    current_user_id: Uuid,
    can_moderate: bool,
    on_changed: EventHandler<()>,
) -> Element {
    let mut editing = use_signal(|| false);
    let mut edit_body = use_signal(|| message.body_markdown.clone().unwrap_or_default());
    let mut error = use_signal(String::new);
    let is_author = message.author_user_id == current_user_id;
    let edit_csrf = csrf_token.clone();
    let delete_csrf = csrf_token.clone();
    let moderation_csrf = csrf_token;
    rsx! {
        article { class: "community-chat-message", aria_label: "Сообщение участника",
            header {
                strong { "{message.author_nickname.as_deref().unwrap_or(\"Без псевдонима\")}" }
                span { " · rev {message.object_revision}" }
            }
            if editing() {
                form {
                    class: "community-comment-form compact",
                    onsubmit: move |event| {
                        event.prevent_default();
                        let csrf = edit_csrf.clone();
                        let request = UpdateSharedChatMessageRequest {
                            body_markdown: edit_body(),
                            expected_revision: message.object_revision,
                        };
                        spawn(async move {
                            match update_space_chat_message(&csrf, space_id, message.id, &request).await {
                                Ok(_) => {
                                    editing.set(false);
                                    on_changed.call(());
                                }
                                Err(update_error) => error.set(update_error.to_string()),
                            }
                        });
                    },
                    label { "Изменить сообщение"
                        textarea {
                            value: "{edit_body}",
                            maxlength: "16384",
                            oninput: move |event| edit_body.set(event.value()),
                        }
                    }
                    div { class: "community-comment-actions",
                        button { r#type: "submit", "Сохранить" }
                        button { r#type: "button", onclick: move |_| editing.set(false), "Отмена" }
                    }
                }
            } else if message.state == SocialContentState::Visible {
                p { class: "community-comment-body", "{message.body_markdown.as_deref().unwrap_or_default()}" }
            } else {
                p { class: "community-comment-placeholder", "{social_state_placeholder(message.state)}" }
            }
            div { class: "community-comment-actions",
                if is_author && message.state == SocialContentState::Visible {
                    button { r#type: "button", onclick: move |_| editing.set(true), "Изменить" }
                    button {
                        r#type: "button",
                        onclick: move |_| {
                            let csrf = delete_csrf.clone();
                            spawn(async move {
                                let request = DeleteSharedChatMessageRequest {
                                    expected_revision: message.object_revision,
                                };
                                match delete_space_chat_message(&csrf, space_id, message.id, &request).await {
                                    Ok(_) => on_changed.call(()),
                                    Err(delete_error) => error.set(delete_error.to_string()),
                                }
                            });
                        },
                        "Удалить"
                    }
                }
                if can_moderate && !is_author && message.state != SocialContentState::Deleted {
                    button {
                        r#type: "button",
                        onclick: move |_| {
                            let csrf = moderation_csrf.clone();
                            let action = if message.state == SocialContentState::Hidden {
                                ModerationActionKind::Restore
                            } else {
                                ModerationActionKind::Hide
                            };
                            spawn(async move {
                                match moderate_social_content(
                                    &csrf,
                                    space_id,
                                    ModerationTargetType::ChatMessage,
                                    message.id,
                                    action,
                                    message.object_revision,
                                ).await {
                                    Ok(_) => on_changed.call(()),
                                    Err(moderation_error) => error.set(moderation_error.to_string()),
                                }
                            });
                        },
                        if message.state == SocialContentState::Hidden { "Восстановить" } else { "Скрыть" }
                    }
                }
            }
            if !error().is_empty() { p { class: "account-error", role: "alert", "{error}" } }
        }
    }
}

#[component]
fn SpaceIdentityEditor(
    csrf_token: String,
    detail: CommunitySpaceDetail,
    community_images_available: bool,
    on_saved: EventHandler<()>,
) -> Element {
    let csrf_token = use_signal(|| csrf_token);
    let mut name = use_signal(|| detail.space.name.clone());
    let mut description = use_signal(|| detail.space.description.clone().unwrap_or_default());
    let mut error = use_signal(String::new);
    rsx! {
        section { class: "library-section community-settings", aria_label: "Настройки сообщества",
            h2 { "Название и описание" }
            label { "Название"
                input { value: "{name}", maxlength: "120", oninput: move |event| name.set(event.value()) }
            }
            label { "Описание"
                textarea { value: "{description}", maxlength: "4096", oninput: move |event| description.set(event.value()) }
            }
            button {
                r#type: "button",
                onclick: move |_| {
                    let csrf = csrf_token.read().clone();
                    let request = UpdateCommunitySpaceRequest {
                        name: name(),
                        description: Some(description()),
                        expected_revision: detail.space.object_revision,
                    };
                    spawn(async move {
                        match update_space(&csrf, detail.space.id, &request).await {
                            Ok(_) => on_saved.call(()),
                            Err(save_error) => error.set(save_error.to_string()),
                        }
                    });
                },
                "Сохранить"
            }
            if community_images_available {
                div { class: "community-image-settings",
                    label { class: "upload-dropzone compact",
                        strong { "Аватар" }
                        small { "PNG/JPEG · до 5 МБ · почти квадратный" }
                        input {
                            r#type: "file",
                            accept: "image/png,image/jpeg",
                            aria_label: "Новый аватар сообщества",
                            onchange: move |event| {
                                let Some(file) = event.files().into_iter().next() else { return; };
                                let csrf = csrf_token.read().clone();
                                spawn(async move {
                                    match file.read_bytes().await {
                                        Ok(bytes) => match replace_space_image(
                                            &csrf,
                                            detail.space.id,
                                            "avatar",
                                            detail.space.object_revision,
                                            bytes.to_vec(),
                                        ).await {
                                            Ok(()) => on_saved.call(()),
                                            Err(upload_error) => error.set(upload_error.to_string()),
                                        },
                                        Err(_) => error.set("Не удалось прочитать изображение.".to_owned()),
                                    }
                                });
                            }
                        }
                    }
                    label { class: "upload-dropzone compact",
                        strong { "Обложка" }
                        small { "PNG/JPEG · до 5 МБ · широкая" }
                        input {
                            r#type: "file",
                            accept: "image/png,image/jpeg",
                            aria_label: "Новая обложка сообщества",
                            onchange: move |event| {
                                let Some(file) = event.files().into_iter().next() else { return; };
                                let csrf = csrf_token.read().clone();
                                spawn(async move {
                                    match file.read_bytes().await {
                                        Ok(bytes) => match replace_space_image(
                                            &csrf,
                                            detail.space.id,
                                            "cover",
                                            detail.space.object_revision,
                                            bytes.to_vec(),
                                        ).await {
                                            Ok(()) => on_saved.call(()),
                                            Err(upload_error) => error.set(upload_error.to_string()),
                                        },
                                        Err(_) => error.set("Не удалось прочитать изображение.".to_owned()),
                                    }
                                });
                            }
                        }
                    }
                    if detail.space.avatar.is_some() {
                        button {
                            class: "text-action danger-text",
                            r#type: "button",
                            onclick: move |_| {
                                let csrf = csrf_token.read().clone();
                                spawn(async move {
                                    match delete_space_image(
                                        &csrf,
                                        detail.space.id,
                                        "avatar",
                                        detail.space.object_revision,
                                    ).await {
                                        Ok(()) => on_saved.call(()),
                                        Err(delete_error) => error.set(delete_error.to_string()),
                                    }
                                });
                            },
                            "Удалить аватар"
                        }
                    }
                    if detail.space.cover.is_some() {
                        button {
                            class: "text-action danger-text",
                            r#type: "button",
                            onclick: move |_| {
                                let csrf = csrf_token.read().clone();
                                spawn(async move {
                                    match delete_space_image(
                                        &csrf,
                                        detail.space.id,
                                        "cover",
                                        detail.space.object_revision,
                                    ).await {
                                        Ok(()) => on_saved.call(()),
                                        Err(delete_error) => error.set(delete_error.to_string()),
                                    }
                                });
                            },
                            "Удалить обложку"
                        }
                    }
                }
            }
            if !error().is_empty() { p { class: "account-error", role: "alert", "{error}" } }
        }
    }
}

#[component]
pub(crate) fn ShareMaterialAction(
    material_id: Uuid,
    csrf_token: String,
    available: bool,
    label: String,
) -> Element {
    let mut open = use_signal(|| false);
    let mut spaces = use_signal(|| Option::<Vec<CommunitySpace>>::None);
    let mut selected = use_signal(|| Option::<CommunitySpace>::None);
    let mut error = use_signal(String::new);
    let mut message = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut idempotency_key = use_signal(String::new);
    if !available {
        return rsx! {};
    }
    rsx! {
        button {
            class: "text-action",
            r#type: "button",
            onclick: move |_| {
                open.set(true);
                selected.set(None);
                error.set(String::new());
                message.set(String::new());
                idempotency_key.set(Uuid::now_v7().to_string());
                spawn(async move {
                    match load_spaces().await {
                        Ok(value) => spaces.set(Some(value)),
                        Err(load_error) => error.set(load_error.to_string()),
                    }
                });
            },
            "{label}"
        }
        if !message().is_empty() {
            span { class: "community-share-status", role: "status", "{message}" }
        }
        if open() {
            dialog { class: "library-dialog community-share-dialog", open: true, aria_modal: "true", aria_label: "Поделиться материалом",
                div { class: "dialog-heading",
                    div {
                        p { class: "eyebrow", "Community" }
                        h2 { "Поделиться материалом" }
                    }
                    button { class: "icon-action", r#type: "button", aria_label: "Закрыть", onclick: move |_| open.set(false), "×" }
                }
                if let Some(space) = selected.read().clone() {
                    p { "Пространство: " strong { "{space.name}" } }
                    p { class: "privacy-note", "Будут опубликованы только название, авторы и защищённый fingerprint. Исходный файл, личные заметки, прогресс и обучение останутся приватными." }
                    div { class: "dialog-actions",
                        button {
                            class: "primary-action",
                            r#type: "button",
                            disabled: busy(),
                            onclick: move |_| {
                                let csrf = csrf_token.clone();
                                let key = idempotency_key();
                                let space_id = space.id;
                                let space_name = space.name.clone();
                                busy.set(true);
                                error.set(String::new());
                                spawn(async move {
                                    match share_private_material(&csrf, space_id, material_id, &key).await {
                                        Ok(shared) => {
                                            message.set(format!("Добавлено в «{}»: {}", space_name, claim_status_label(shared.claim.as_ref().map(|claim| claim.status))));
                                            open.set(false);
                                        }
                                        Err(share_error) => error.set(share_error.to_string()),
                                    }
                                    busy.set(false);
                                });
                            },
                            if busy() { "Публикуем…" } else { "Поделиться" }
                        }
                        button { class: "secondary-action", r#type: "button", onclick: move |_| selected.set(None), "Назад" }
                    }
                } else {
                    p { "Выберите одно пространство." }
                    match spaces.read().clone() {
                        None => rsx! { p { role: "status", "Загружаем пространства…" } },
                        Some(values) if values.is_empty() => rsx! { p { "Сначала создайте сообщество или вступите по ссылке." } },
                        Some(values) => rsx! {
                            ul { class: "community-space-picker",
                                for space in values {
                                    li {
                                        button { r#type: "button", onclick: move |_| selected.set(Some(space.clone())), "{space.name}" }
                                    }
                                }
                            }
                        },
                    }
                }
                if !error().is_empty() {
                    p { class: "account-error", role: "alert", "{error}" }
                }
            }
        }
    }
}

#[component]
fn CommunityMaterialCard(
    material: SharedMaterial,
    space_id: Uuid,
    csrf_token: String,
    current_user_id: Uuid,
    can_moderate: bool,
    discussions_available: bool,
    on_changed: EventHandler<()>,
) -> Element {
    let creators = if material.identity.creators.is_empty() {
        "Автор не указан".to_owned()
    } else {
        material.identity.creators.join(", ")
    };
    let claim = material.claim.clone();
    rsx! {
        article { class: "community-material-card", aria_label: "Материал сообщества {material.identity.canonical_title}",
            p { class: "eyebrow", "{claim_status_label(claim.as_ref().map(|value| value.status))}" }
            h3 { "{material.identity.canonical_title}" }
            p { "{creators}" }
            p { class: "material-meta", "Форматы копий: {source_formats_label(&material.identity.source_formats)}" }
            if claim.as_ref().is_some_and(|value| matches!(value.status, UserMaterialClaimStatus::ManualReview | UserMaterialClaimStatus::Rejected)) {
                button {
                    class: "secondary-action",
                    r#type: "button",
                    onclick: {
                        let csrf = csrf_token.clone();
                        let shared_material_id = material.identity.id;
                        move |_| {
                            let csrf = csrf.clone();
                            spawn(async move {
                                if recheck_claim(&csrf, space_id, shared_material_id).await.is_ok() {
                                    on_changed.call(());
                                }
                            });
                        }
                    },
                    "Проверить снова"
                }
            }
            if claim.as_ref().is_none_or(|value| value.status != UserMaterialClaimStatus::Matched) {
                ClaimMaterialAction {
                    space_id,
                    shared_material_id: material.identity.id,
                    csrf_token: csrf_token.clone(),
                    on_changed,
                }
            }
            if discussions_available {
                DiscussionPanel {
                    title: material.identity.canonical_title.clone(),
                    space_id,
                    shared_material_id: material.identity.id,
                    csrf_token,
                    current_user_id,
                    can_moderate,
                }
            }
        }
    }
}

#[component]
fn DiscussionPanel(
    title: String,
    space_id: Uuid,
    shared_material_id: Uuid,
    csrf_token: String,
    current_user_id: Uuid,
    can_moderate: bool,
) -> Element {
    let mut open = use_signal(|| false);
    let mut page = use_signal(|| Option::<SharedDiscussionPage>::None);
    let mut new_body = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut generation = use_signal(|| 0_u64);
    let mut poll_ticket = use_signal(|| 0_u64);
    use_effect(move || {
        let _ = generation();
        let next_ticket = poll_ticket.peek().saturating_add(1);
        poll_ticket.set(next_ticket);
        if !open() {
            return;
        }
        spawn(async move {
            match load_discussions(space_id, shared_material_id, None).await {
                Ok(loaded) => {
                    let current_page = page.peek().clone();
                    page.set(Some(merge_discussion_pages(current_page, loaded, true)));
                    error.set(String::new());
                    schedule_visible_refresh(generation, poll_ticket, 5_000);
                }
                Err(load_error) => {
                    error.set(load_error.to_string());
                    schedule_visible_refresh(generation, poll_ticket, 15_000);
                }
            }
        });
    });
    rsx! {
        button {
            class: "text-action",
            r#type: "button",
            aria_expanded: open(),
            onclick: move |_| open.toggle(),
            if open() { "Скрыть обсуждение" } else { "Открыть обсуждение" }
        }
        if open() {
            section {
                class: "community-discussion",
                aria_label: "Обсуждение материала {title}",
                h4 { "Обсуждение" }
                p { class: "community-discussion-note", "В этом срезе комментарии относятся ко всему материалу и не содержат цитат из личных копий." }
                if !error().is_empty() {
                    div { class: "library-alert", role: "alert",
                        p { "{error}" }
                        button { r#type: "button", onclick: move |_| generation += 1, "Повторить" }
                    }
                }
                form {
                    class: "community-comment-form",
                    onsubmit: move |event| {
                        event.prevent_default();
                        let body = new_body();
                        if body.trim().is_empty() {
                            return;
                        }
                        let csrf = csrf_token.clone();
                        busy.set(true);
                        spawn(async move {
                            match create_shared_thread(&csrf, space_id, shared_material_id, body).await {
                                Ok(_) => {
                                    new_body.set(String::new());
                                    generation += 1;
                                }
                                Err(create_error) => error.set(create_error.to_string()),
                            }
                            busy.set(false);
                        });
                    },
                    label { "Начать новое обсуждение"
                        textarea {
                            value: "{new_body}",
                            maxlength: "16384",
                            oninput: move |event| new_body.set(event.value()),
                        }
                    }
                    button {
                        class: "primary-action",
                        r#type: "submit",
                        disabled: busy() || new_body().trim().is_empty(),
                        if busy() { "Публикуем…" } else { "Опубликовать" }
                    }
                }
                match page.read().clone() {
                    None => rsx! { p { role: "status", "Загружаем обсуждение…" } },
                    Some(value) if value.threads.is_empty() => rsx! { p { "Пока нет обсуждений." } },
                    Some(value) => rsx! {
                        div { class: "community-thread-list",
                            for thread in value.threads {
                                DiscussionThreadView {
                                    thread,
                                    space_id,
                                    csrf_token: csrf_token.clone(),
                                    current_user_id,
                                    can_moderate,
                                    on_changed: move |_| generation += 1,
                                }
                            }
                        }
                        if let Some(cursor) = value.next_cursor {
                            button {
                                class: "secondary-action",
                                r#type: "button",
                                onclick: move |_| {
                                    let cursor = cursor.clone();
                                    spawn(async move {
                                        match load_discussions(space_id, shared_material_id, Some(&cursor)).await {
                                            Ok(mut loaded) => {
                                                let current_page = { page.read().clone() };
                                                if let Some(mut current) = current_page {
                                                    for thread in loaded.threads.drain(..) {
                                                        if let Some(index) = current.threads.iter().position(|item| item.id == thread.id) {
                                                            current.threads[index] = thread;
                                                        } else {
                                                            current.threads.push(thread);
                                                        }
                                                    }
                                                    current.next_cursor = loaded.next_cursor;
                                                    page.set(Some(current));
                                                }
                                            }
                                            Err(load_error) => error.set(load_error.to_string()),
                                        }
                                    });
                                },
                                "Загрузить ещё"
                            }
                        }
                    },
                }
            }
        }
    }
}

#[component]
fn DiscussionThreadView(
    thread: SharedCommentThread,
    space_id: Uuid,
    csrf_token: String,
    current_user_id: Uuid,
    can_moderate: bool,
    on_changed: EventHandler<()>,
) -> Element {
    let mut reply_to = use_signal(|| Option::<Uuid>::None);
    let mut reply_body = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    rsx! {
        article { class: "community-thread", aria_label: "Ветка обсуждения",
            header { class: "community-thread-heading",
                strong { "{thread.creator_nickname.as_deref().unwrap_or(\"Участник\")}" }
                span { "{social_state_label(thread.state)}" }
            }
            if can_moderate && thread.state != SocialContentState::Deleted {
                div { class: "community-comment-actions",
                    if thread.state == SocialContentState::Visible {
                        button {
                            r#type: "button",
                            onclick: {
                                let csrf = csrf_token.clone();
                                let thread = thread.clone();
                                move |_| {
                                    let csrf = csrf.clone();
                                    let thread = thread.clone();
                                    spawn(async move {
                                        match moderate_social_content(
                                            &csrf,
                                            space_id,
                                            ModerationTargetType::Thread,
                                            thread.id,
                                            ModerationActionKind::Hide,
                                            thread.object_revision,
                                        ).await {
                                            Ok(_) => on_changed.call(()),
                                            Err(action_error) => error.set(action_error.to_string()),
                                        }
                                    });
                                }
                            },
                            "Скрыть ветку"
                        }
                    } else if thread.state == SocialContentState::Hidden {
                        button {
                            r#type: "button",
                            onclick: {
                                let csrf = csrf_token.clone();
                                let thread = thread.clone();
                                move |_| {
                                    let csrf = csrf.clone();
                                    let thread = thread.clone();
                                    spawn(async move {
                                        match moderate_social_content(
                                            &csrf,
                                            space_id,
                                            ModerationTargetType::Thread,
                                            thread.id,
                                            ModerationActionKind::Restore,
                                            thread.object_revision,
                                        ).await {
                                            Ok(_) => on_changed.call(()),
                                            Err(action_error) => error.set(action_error.to_string()),
                                        }
                                    });
                                }
                            },
                            "Восстановить ветку"
                        }
                    }
                    button {
                        class: "danger-action",
                        r#type: "button",
                        onclick: {
                            let csrf = csrf_token.clone();
                            let thread = thread.clone();
                            move |_| {
                                let csrf = csrf.clone();
                                let thread = thread.clone();
                                spawn(async move {
                                    match moderate_social_content(
                                        &csrf,
                                        space_id,
                                        ModerationTargetType::Thread,
                                        thread.id,
                                        ModerationActionKind::Delete,
                                        thread.object_revision,
                                    ).await {
                                        Ok(_) => on_changed.call(()),
                                        Err(action_error) => error.set(action_error.to_string()),
                                    }
                                });
                            }
                        },
                        "Удалить ветку"
                    }
                }
            }
            ol { class: "community-comment-list",
                for comment in thread.comments.clone() {
                    li { class: if comment.parent_comment_id.is_some() { "community-comment is-reply" } else { "community-comment" },
                        DiscussionCommentView {
                            comment: comment.clone(),
                            space_id,
                            csrf_token: csrf_token.clone(),
                            current_user_id,
                            can_moderate,
                            on_reply: move |comment_id| reply_to.set(Some(comment_id)),
                            on_changed,
                        }
                    }
                }
            }
            if thread.state == SocialContentState::Visible {
                form {
                    class: "community-comment-form compact",
                    onsubmit: {
                        let thread_id = thread.id;
                        move |event| {
                            event.prevent_default();
                            let body = reply_body();
                            if body.trim().is_empty() {
                                return;
                            }
                            let csrf = csrf_token.clone();
                            let parent_comment_id = reply_to();
                            busy.set(true);
                            spawn(async move {
                                match create_shared_comment(
                                    &csrf,
                                    space_id,
                                    thread_id,
                                    parent_comment_id,
                                    body,
                                ).await {
                                    Ok(_) => {
                                        reply_body.set(String::new());
                                        reply_to.set(None);
                                        on_changed.call(());
                                    }
                                    Err(create_error) => error.set(create_error.to_string()),
                                }
                                busy.set(false);
                            });
                        }
                    },
                    label {
                        if reply_to().is_some() { "Ответить на комментарий" } else { "Добавить комментарий" }
                        textarea {
                            value: "{reply_body}",
                            maxlength: "16384",
                            oninput: move |event| reply_body.set(event.value()),
                        }
                    }
                    div { class: "community-comment-actions",
                        button {
                            r#type: "submit",
                            disabled: busy() || reply_body().trim().is_empty(),
                            if busy() { "Отправляем…" } else if reply_to().is_some() { "Ответить" } else { "Добавить" }
                        }
                        if reply_to().is_some() {
                            button { r#type: "button", onclick: move |_| reply_to.set(None), "Отменить ответ" }
                        }
                    }
                }
            }
            if !error().is_empty() {
                p { class: "account-error", role: "alert", "{error}" }
            }
        }
    }
}

#[component]
fn DiscussionCommentView(
    comment: SharedComment,
    space_id: Uuid,
    csrf_token: String,
    current_user_id: Uuid,
    can_moderate: bool,
    on_reply: EventHandler<Uuid>,
    on_changed: EventHandler<()>,
) -> Element {
    let mut editing = use_signal(|| false);
    let mut edited_body = use_signal(|| comment.body_markdown.clone().unwrap_or_default());
    let mut error = use_signal(String::new);
    let authored_by_current_user = comment.author_user_id == current_user_id;
    let csrf_for_edit = csrf_token.clone();
    let csrf_for_delete = csrf_token.clone();
    let csrf_for_hide = csrf_token.clone();
    let csrf_for_restore = csrf_token;
    rsx! {
        article { aria_label: "Комментарий {comment.author_nickname.as_deref().unwrap_or(\"участника\")}",
            header { class: "community-comment-heading",
                strong { "{comment.author_nickname.as_deref().unwrap_or(\"Участник\")}" }
                span { "{social_state_label(comment.state)}" }
            }
            if editing() {
                form {
                    class: "community-comment-form compact",
                    onsubmit: {
                        let comment = comment.clone();
                        move |event| {
                            event.prevent_default();
                            let body = edited_body();
                            let csrf = csrf_for_edit.clone();
                            let comment = comment.clone();
                            spawn(async move {
                                match update_shared_comment(
                                    &csrf,
                                    space_id,
                                    comment.id,
                                    comment.object_revision,
                                    body,
                                ).await {
                                    Ok(_) => {
                                        editing.set(false);
                                        on_changed.call(());
                                    }
                                    Err(update_error) => error.set(update_error.to_string()),
                                }
                            });
                        }
                    },
                    label { "Текст комментария"
                        textarea {
                            value: "{edited_body}",
                            maxlength: "16384",
                            oninput: move |event| edited_body.set(event.value()),
                        }
                    }
                    div { class: "community-comment-actions",
                        button { r#type: "submit", disabled: edited_body().trim().is_empty(), "Сохранить" }
                        button { r#type: "button", onclick: move |_| editing.set(false), "Отмена" }
                    }
                }
            } else if let Some(body) = &comment.body_markdown {
                p { class: "community-comment-body", "{body}" }
            } else {
                p { class: "community-comment-placeholder", "{social_state_placeholder(comment.state)}" }
            }
            div { class: "community-comment-actions",
                if comment.parent_comment_id.is_none() && comment.state == SocialContentState::Visible {
                    button { r#type: "button", onclick: move |_| on_reply.call(comment.id), "Ответить" }
                }
                if authored_by_current_user && comment.state == SocialContentState::Visible {
                    button { r#type: "button", onclick: move |_| editing.set(true), "Изменить" }
                    button {
                        class: "danger-action",
                        r#type: "button",
                        onclick: {
                            let comment = comment.clone();
                            move |_| {
                                let csrf = csrf_for_delete.clone();
                                let comment = comment.clone();
                                spawn(async move {
                                    match delete_shared_comment(
                                        &csrf,
                                        space_id,
                                        comment.id,
                                        comment.object_revision,
                                    ).await {
                                        Ok(_) => on_changed.call(()),
                                        Err(delete_error) => error.set(delete_error.to_string()),
                                    }
                                });
                            }
                        },
                        "Удалить"
                    }
                }
                if can_moderate && comment.state != SocialContentState::Deleted {
                    if comment.state == SocialContentState::Visible {
                        button {
                            r#type: "button",
                            onclick: {
                                let comment = comment.clone();
                                move |_| {
                                    let csrf = csrf_for_hide.clone();
                                    let comment = comment.clone();
                                    spawn(async move {
                                        match moderate_social_content(
                                            &csrf,
                                            space_id,
                                            ModerationTargetType::Comment,
                                            comment.id,
                                            ModerationActionKind::Hide,
                                            comment.object_revision,
                                        ).await {
                                            Ok(_) => on_changed.call(()),
                                            Err(action_error) => error.set(action_error.to_string()),
                                        }
                                    });
                                }
                            },
                            "Скрыть"
                        }
                    } else if comment.state == SocialContentState::Hidden {
                        button {
                            r#type: "button",
                            onclick: {
                                let comment = comment.clone();
                                move |_| {
                                    let csrf = csrf_for_restore.clone();
                                    let comment = comment.clone();
                                    spawn(async move {
                                        match moderate_social_content(
                                            &csrf,
                                            space_id,
                                            ModerationTargetType::Comment,
                                            comment.id,
                                            ModerationActionKind::Restore,
                                            comment.object_revision,
                                        ).await {
                                            Ok(_) => on_changed.call(()),
                                            Err(action_error) => error.set(action_error.to_string()),
                                        }
                                    });
                                }
                            },
                            "Восстановить"
                        }
                    }
                }
            }
            if !error().is_empty() {
                p { class: "account-error", role: "alert", "{error}" }
            }
        }
    }
}

#[component]
fn ClaimMaterialAction(
    space_id: Uuid,
    shared_material_id: Uuid,
    csrf_token: String,
    on_changed: EventHandler<()>,
) -> Element {
    let mut open = use_signal(|| false);
    let mut library = use_signal(|| Option::<Vec<LibraryEntry>>::None);
    let mut error = use_signal(String::new);
    rsx! {
        button {
            class: "text-action",
            r#type: "button",
            onclick: move |_| {
                open.set(true);
                error.set(String::new());
                spawn(async move {
                    match load_library_materials().await {
                        Ok(entries) => library.set(Some(entries)),
                        Err(load_error) => error.set(load_error.to_string()),
                    }
                });
            },
            "Подключить свою копию"
        }
        if open() {
            dialog { class: "library-dialog community-claim-dialog", open: true, aria_modal: "true", aria_label: "Подключить свою копию",
                div { class: "dialog-heading",
                    h2 { "Выберите материал из своей библиотеки" }
                    button { class: "icon-action", r#type: "button", aria_label: "Закрыть", onclick: move |_| open.set(false), "×" }
                }
                p { class: "privacy-note", "Lumi сравнит копии на сервере. Файл и полный текст не публикуются в сообщество." }
                match library.read().clone() {
                    None => rsx! { p { role: "status", "Загружаем библиотеку…" } },
                    Some(entries) => rsx! {
                        ul { class: "community-space-picker",
                            for entry in entries.into_iter().filter(|entry| entry.import_status == MaterialImportStatus::Ready) {
                                li {
                                    button {
                                        r#type: "button",
                                        onclick: {
                                            let csrf = csrf_token.clone();
                                            move |_| {
                                                let csrf = csrf.clone();
                                                spawn(async move {
                                                    match claim_private_material(&csrf, space_id, shared_material_id, entry.id).await {
                                                        Ok(_) => {
                                                            open.set(false);
                                                            on_changed.call(());
                                                        }
                                                        Err(claim_error) => error.set(claim_error.to_string()),
                                                    }
                                                });
                                            }
                                        },
                                        "{entry.display_title()}"
                                    }
                                }
                            }
                        }
                    },
                }
                if !error().is_empty() { p { class: "account-error", role: "alert", "{error}" } }
            }
        }
    }
}

async fn load_spaces() -> Result<Vec<CommunitySpace>, ApiError> {
    let response = Request::get(&format!("{API_BASE}/spaces"))
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(network_error)?;
    parse_json(response).await
}

async fn load_shared_materials(space_id: Uuid) -> Result<Vec<SharedMaterial>, ApiError> {
    api_get(&format!("/spaces/{space_id}/materials")).await
}

async fn load_discussions(
    space_id: Uuid,
    shared_material_id: Uuid,
    after: Option<&str>,
) -> Result<SharedDiscussionPage, ApiError> {
    let cursor = after.map_or_else(String::new, |value| format!("&after={value}"));
    api_get(&format!(
        "/spaces/{space_id}/materials/{shared_material_id}/threads?limit=100{cursor}"
    ))
    .await
}

async fn load_space_chat(space_id: Uuid, after: Option<&str>) -> Result<SharedChatPage, ApiError> {
    let suffix = after.map_or(String::new(), |cursor| format!("?after={cursor}"));
    api_get(&format!("/spaces/{space_id}/chat{suffix}")).await
}

async fn create_space_chat_message(
    csrf: &str,
    space_id: Uuid,
    body_markdown: &str,
) -> Result<SharedChatMessage, ApiError> {
    api_json_mutation(
        "POST",
        &format!("/spaces/{space_id}/chat"),
        csrf,
        &CreateSharedChatMessageRequest {
            body_markdown: body_markdown.to_owned(),
        },
    )
    .await
}

async fn update_space_chat_message(
    csrf: &str,
    space_id: Uuid,
    message_id: Uuid,
    request: &UpdateSharedChatMessageRequest,
) -> Result<SharedChatMessage, ApiError> {
    api_json_mutation(
        "PATCH",
        &format!("/spaces/{space_id}/chat/{message_id}"),
        csrf,
        request,
    )
    .await
}

async fn delete_space_chat_message(
    csrf: &str,
    space_id: Uuid,
    message_id: Uuid,
    request: &DeleteSharedChatMessageRequest,
) -> Result<SharedChatMessage, ApiError> {
    api_json_mutation(
        "DELETE",
        &format!("/spaces/{space_id}/chat/{message_id}"),
        csrf,
        request,
    )
    .await
}

async fn load_space_activity(
    space_id: Uuid,
    after: Option<&str>,
) -> Result<CommunityActivityPage, ApiError> {
    let suffix = after.map_or(String::new(), |cursor| format!("?after={cursor}"));
    api_get(&format!("/spaces/{space_id}/activity{suffix}")).await
}

async fn create_shared_thread(
    csrf: &str,
    space_id: Uuid,
    shared_material_id: Uuid,
    body_markdown: String,
) -> Result<SharedCommentThread, ApiError> {
    api_json_mutation(
        "POST",
        &format!("/spaces/{space_id}/materials/{shared_material_id}/threads"),
        csrf,
        &CreateSharedThreadRequest {
            body_markdown,
            target: Default::default(),
        },
    )
    .await
}

async fn create_shared_comment(
    csrf: &str,
    space_id: Uuid,
    thread_id: Uuid,
    parent_comment_id: Option<Uuid>,
    body_markdown: String,
) -> Result<SharedComment, ApiError> {
    api_json_mutation(
        "POST",
        &format!("/spaces/{space_id}/threads/{thread_id}/comments"),
        csrf,
        &CreateSharedCommentRequest {
            parent_comment_id,
            body_markdown,
        },
    )
    .await
}

async fn update_shared_comment(
    csrf: &str,
    space_id: Uuid,
    comment_id: Uuid,
    expected_revision: u64,
    body_markdown: String,
) -> Result<SharedComment, ApiError> {
    api_json_mutation(
        "PATCH",
        &format!("/spaces/{space_id}/comments/{comment_id}"),
        csrf,
        &UpdateSharedCommentRequest {
            body_markdown,
            expected_revision,
        },
    )
    .await
}

async fn delete_shared_comment(
    csrf: &str,
    space_id: Uuid,
    comment_id: Uuid,
    expected_revision: u64,
) -> Result<SharedComment, ApiError> {
    api_json_mutation(
        "DELETE",
        &format!("/spaces/{space_id}/comments/{comment_id}"),
        csrf,
        &DeleteSharedCommentRequest { expected_revision },
    )
    .await
}

async fn moderate_social_content(
    csrf: &str,
    space_id: Uuid,
    target_type: ModerationTargetType,
    target_id: Uuid,
    action: ModerationActionKind,
    expected_revision: u64,
) -> Result<ModerationAction, ApiError> {
    api_json_mutation(
        "POST",
        &format!("/spaces/{space_id}/moderation/actions"),
        csrf,
        &ModerateSocialContentRequest {
            target_type,
            target_id,
            action,
            expected_revision,
            reason: None,
        },
    )
    .await
}

async fn load_library_materials() -> Result<Vec<LibraryEntry>, ApiError> {
    api_get("/materials").await
}

async fn share_private_material(
    csrf: &str,
    space_id: Uuid,
    material_id: Uuid,
    idempotency_key: &str,
) -> Result<SharedMaterial, ApiError> {
    api_json_mutation_with_key(
        "POST",
        &format!("/spaces/{space_id}/materials/share"),
        csrf,
        idempotency_key,
        &ShareMaterialRequest { material_id },
    )
    .await
}

async fn claim_private_material(
    csrf: &str,
    space_id: Uuid,
    shared_material_id: Uuid,
    material_id: Uuid,
) -> Result<SharedMaterial, ApiError> {
    api_json_mutation(
        "POST",
        &format!("/spaces/{space_id}/materials/{shared_material_id}/claim"),
        csrf,
        &ClaimSharedMaterialRequest { material_id },
    )
    .await
}

async fn recheck_claim(
    csrf: &str,
    space_id: Uuid,
    shared_material_id: Uuid,
) -> Result<SharedMaterial, ApiError> {
    api_empty_mutation_json(
        "POST",
        &format!("/spaces/{space_id}/materials/{shared_material_id}/claim/recheck"),
        csrf,
    )
    .await
}

async fn load_space_bundle(
    space_id: Uuid,
) -> Result<
    (
        CommunitySpaceDetail,
        Vec<CommunityMembership>,
        Vec<CommunityAccessLink>,
    ),
    ApiError,
> {
    let detail: CommunitySpaceDetail = api_get(&format!("/spaces/{space_id}")).await?;
    let members: Vec<CommunityMembership> = api_get(&format!("/spaces/{space_id}/members")).await?;
    let links = if detail.permissions.can_manage_links {
        api_get(&format!("/spaces/{space_id}/access-links")).await?
    } else {
        Vec::new()
    };
    Ok((detail, members, links))
}

async fn create_space(
    csrf: &str,
    request: &CreateCommunitySpaceRequest,
) -> Result<CommunitySpaceDetail, ApiError> {
    api_json_mutation("POST", "/spaces", csrf, request).await
}

async fn update_space(
    csrf: &str,
    space_id: Uuid,
    request: &UpdateCommunitySpaceRequest,
) -> Result<CommunitySpaceDetail, ApiError> {
    api_json_mutation("PATCH", &format!("/spaces/{space_id}"), csrf, request).await
}

async fn replace_space_image(
    csrf: &str,
    space_id: Uuid,
    kind: &str,
    expected_revision: u64,
    bytes: Vec<u8>,
) -> Result<(), ApiError> {
    if bytes.is_empty() || bytes.len() > lumi_core::COMMUNITY_IMAGE_MAX_BYTES {
        return Err(ApiError::Message(
            "Изображение пустое или превышает лимит 5 МБ.".to_owned(),
        ));
    }
    let media_type = image_media_type(&bytes).ok_or_else(|| {
        ApiError::Message("Поддерживаются только корректные PNG и JPEG.".to_owned())
    })?;
    let response = Request::put(&format!(
        "{API_BASE}/spaces/{space_id}/images/{kind}?expected_revision={expected_revision}"
    ))
    .credentials(RequestCredentials::Include)
    .header("X-Lumi-CSRF", csrf)
    .header("Content-Type", media_type)
    .body(bytes)
    .map_err(network_error)?
    .send()
    .await
    .map_err(network_error)?;
    if response.status() == 401 {
        crate::account::notify_session_expired();
        return Err(ApiError::Unauthorized);
    }
    if response.ok() {
        Ok(())
    } else {
        Err(api_response_error(&response))
    }
}

async fn delete_space_image(
    csrf: &str,
    space_id: Uuid,
    kind: &str,
    expected_revision: u64,
) -> Result<(), ApiError> {
    let response = Request::delete(&format!(
        "{API_BASE}/spaces/{space_id}/images/{kind}?expected_revision={expected_revision}"
    ))
    .credentials(RequestCredentials::Include)
    .header("X-Lumi-CSRF", csrf)
    .send()
    .await
    .map_err(network_error)?;
    if response.status() == 401 {
        crate::account::notify_session_expired();
        return Err(ApiError::Unauthorized);
    }
    if response.ok() {
        Ok(())
    } else {
        Err(api_response_error(&response))
    }
}

fn image_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else {
        None
    }
}

async fn preview_link(token: &str) -> Result<CommunityLinkPreview, ApiError> {
    let request = Request::post(&format!("{API_BASE}/shares/community-link/preview"))
        .credentials(RequestCredentials::Include)
        .json(&PreviewCommunityLinkRequest {
            token: token.to_owned(),
        })
        .map_err(network_error)?;
    parse_json(request.send().await.map_err(network_error)?).await
}

async fn join_space(csrf: &str, token: &str) -> Result<CommunitySpaceDetail, ApiError> {
    api_json_mutation(
        "POST",
        "/shares/community-link/join",
        csrf,
        &JoinCommunityLinkRequest {
            token: token.to_owned(),
        },
    )
    .await
}

async fn create_access_link(
    csrf: &str,
    space_id: Uuid,
) -> Result<CreatedCommunityAccessLink, ApiError> {
    api_json_mutation(
        "POST",
        &format!("/spaces/{space_id}/access-links"),
        csrf,
        &CreateCommunityAccessLinkRequest {
            expires_at: None,
            max_uses: None,
        },
    )
    .await
}

async fn rotate_access_link(
    csrf: &str,
    space_id: Uuid,
    link_id: Uuid,
) -> Result<CreatedCommunityAccessLink, ApiError> {
    api_empty_mutation_json(
        "POST",
        &format!("/spaces/{space_id}/access-links/{link_id}/rotate"),
        csrf,
    )
    .await
}

async fn revoke_access_link(csrf: &str, space_id: Uuid, link_id: Uuid) -> Result<(), ApiError> {
    api_empty_mutation(
        "DELETE",
        &format!("/spaces/{space_id}/access-links/{link_id}"),
        csrf,
    )
    .await
}

async fn change_member_role(
    csrf: &str,
    space_id: Uuid,
    member: &CommunityMembership,
    role: CommunityRole,
) -> Result<CommunityMembership, ApiError> {
    api_json_mutation(
        "PATCH",
        &format!("/spaces/{space_id}/members/{}", member.user_id),
        csrf,
        &UpdateCommunityMemberRequest {
            role,
            expected_revision: member.object_revision,
        },
    )
    .await
}

async fn remove_member(csrf: &str, space_id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
    api_empty_mutation(
        "DELETE",
        &format!("/spaces/{space_id}/members/{user_id}"),
        csrf,
    )
    .await
}

async fn leave_space(csrf: &str, space_id: Uuid) -> Result<(), ApiError> {
    api_empty_mutation("POST", &format!("/spaces/{space_id}/leave"), csrf).await
}

async fn api_get<T>(path: &str) -> Result<T, ApiError>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let response = Request::get(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(network_error)?;
    parse_json(response).await
}

async fn api_json_mutation<T, R>(
    method: &str,
    path: &str,
    csrf: &str,
    body: &T,
) -> Result<R, ApiError>
where
    T: serde::Serialize + ?Sized,
    R: for<'de> serde::Deserialize<'de>,
{
    api_json_mutation_with_key(method, path, csrf, &Uuid::now_v7().to_string(), body).await
}

async fn api_json_mutation_with_key<T, R>(
    method: &str,
    path: &str,
    csrf: &str,
    idempotency_key: &str,
    body: &T,
) -> Result<R, ApiError>
where
    T: serde::Serialize + ?Sized,
    R: for<'de> serde::Deserialize<'de>,
{
    let builder = match method {
        "POST" => Request::post(&format!("{API_BASE}{path}")),
        "PATCH" => Request::patch(&format!("{API_BASE}{path}")),
        "DELETE" => Request::delete(&format!("{API_BASE}{path}")),
        _ => return Err(ApiError::Message("Неподдерживаемая операция.".to_owned())),
    };
    let request = builder
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .header("Idempotency-Key", idempotency_key)
        .json(body)
        .map_err(network_error)?;
    parse_json(request.send().await.map_err(network_error)?).await
}

async fn api_empty_mutation_json<R>(method: &str, path: &str, csrf: &str) -> Result<R, ApiError>
where
    R: for<'de> serde::Deserialize<'de>,
{
    let response = mutation_builder(method, path)?
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .header("Idempotency-Key", &Uuid::now_v7().to_string())
        .send()
        .await
        .map_err(network_error)?;
    parse_json(response).await
}

async fn api_empty_mutation(method: &str, path: &str, csrf: &str) -> Result<(), ApiError> {
    let response = mutation_builder(method, path)?
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

fn mutation_builder(method: &str, path: &str) -> Result<gloo_net::http::RequestBuilder, ApiError> {
    let url = format!("{API_BASE}{path}");
    match method {
        "POST" => Ok(Request::post(&url)),
        "DELETE" => Ok(Request::delete(&url)),
        _ => Err(ApiError::Message("Неподдерживаемая операция.".to_owned())),
    }
}

fn join_url(token: &str) -> String {
    web_sys::window()
        .and_then(|window| window.location().origin().ok())
        .map_or_else(
            || format!("#join/{token}"),
            |origin| format!("{origin}/#join/{token}"),
        )
}

fn copy_to_clipboard(value: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.navigator().clipboard().write_text(value);
    }
}

fn role_label(role: CommunityRole) -> &'static str {
    match role {
        CommunityRole::Owner => "Владелец",
        CommunityRole::Admin => "Администратор",
        CommunityRole::Member => "Участник",
    }
}

fn claim_status_label(status: Option<UserMaterialClaimStatus>) -> &'static str {
    match status {
        Some(UserMaterialClaimStatus::Matched) => "Есть ваша копия",
        Some(UserMaterialClaimStatus::Pending) => "Ищем совпадение",
        Some(UserMaterialClaimStatus::ManualReview) => "Нужно подтвердить",
        Some(UserMaterialClaimStatus::Rejected) => "Копия не совпала",
        None => "Импортируйте свою копию",
    }
}

fn source_formats_label(formats: &[lumi_core::SourceFormat]) -> String {
    formats
        .iter()
        .map(|format| match format {
            lumi_core::SourceFormat::Epub => "EPUB",
            lumi_core::SourceFormat::Pdf => "PDF",
            lumi_core::SourceFormat::WebPage => "Web",
            lumi_core::SourceFormat::Telegram => "Telegram",
            lumi_core::SourceFormat::Markdown => "Markdown",
            lumi_core::SourceFormat::Lum => "LUM",
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn link_status_label(status: CommunityAccessLinkStatus) -> &'static str {
    match status {
        CommunityAccessLinkStatus::Active => "Активна",
        CommunityAccessLinkStatus::Revoked => "Отозвана",
    }
}

fn social_state_label(state: SocialContentState) -> &'static str {
    match state {
        SocialContentState::Visible => "Опубликовано",
        SocialContentState::Hidden => "Скрыто модератором",
        SocialContentState::Deleted => "Удалено",
    }
}

fn social_state_placeholder(state: SocialContentState) -> &'static str {
    match state {
        SocialContentState::Visible => "",
        SocialContentState::Hidden => "Содержимое скрыто модератором.",
        SocialContentState::Deleted => "Комментарий удалён.",
    }
}

fn activity_label(event: &CommunityActivityEvent) -> &'static str {
    match event.kind {
        CommunityActivityKind::SpaceCreated => "создал(а) пространство",
        CommunityActivityKind::MemberJoined => "вступил(а) в пространство",
        CommunityActivityKind::MemberLeft => "вышел/вышла из пространства",
        CommunityActivityKind::MemberRemoved => "удалил(а) участника",
        CommunityActivityKind::MaterialAdded => "добавил(а) материал",
        CommunityActivityKind::DiscussionStarted => "начал(а) обсуждение материала",
        CommunityActivityKind::ContentModerated => "изменил(а) видимость публикации",
        CommunityActivityKind::ChatMessageCreated => "написал(а) в чат",
    }
}

fn merge_chat_pages(
    existing: Option<SharedChatPage>,
    incoming: SharedChatPage,
    preserve_existing_cursor: bool,
) -> SharedChatPage {
    let Some(mut existing) = existing else {
        return incoming;
    };
    let incoming_len = incoming.messages.len();
    for message in incoming.messages {
        if let Some(position) = existing
            .messages
            .iter()
            .position(|candidate| candidate.id == message.id)
        {
            existing.messages[position] = message;
        } else {
            existing.messages.push(message);
        }
    }
    existing
        .messages
        .sort_by_key(|message| (message.created_at, message.id));
    if !preserve_existing_cursor || existing.messages.len() <= incoming_len {
        existing.next_cursor = incoming.next_cursor;
    }
    existing
}

fn merge_discussion_pages(
    existing: Option<SharedDiscussionPage>,
    mut incoming: SharedDiscussionPage,
    preserve_existing_cursor: bool,
) -> SharedDiscussionPage {
    let Some(mut existing) = existing else {
        return incoming;
    };
    let incoming_len = incoming.threads.len();
    for thread in incoming.threads.drain(..) {
        if let Some(position) = existing
            .threads
            .iter()
            .position(|candidate| candidate.id == thread.id)
        {
            existing.threads[position] = thread;
        } else {
            existing.threads.push(thread);
        }
    }
    existing
        .threads
        .sort_by_key(|thread| (thread.created_at, thread.id));
    if !preserve_existing_cursor || existing.threads.len() <= incoming_len {
        existing.next_cursor = incoming.next_cursor;
    }
    existing
}

fn merge_activity_pages(
    existing: Option<CommunityActivityPage>,
    incoming: CommunityActivityPage,
    preserve_existing_cursor: bool,
) -> CommunityActivityPage {
    let Some(mut existing) = existing else {
        return incoming;
    };
    let incoming_len = incoming.events.len();
    for event in incoming.events {
        if !existing
            .events
            .iter()
            .any(|candidate| candidate.id == event.id)
        {
            existing.events.push(event);
        }
    }
    existing
        .events
        .sort_by_key(|event| (event.created_at, event.id));
    if !preserve_existing_cursor || existing.events.len() <= incoming_len {
        existing.next_cursor = incoming.next_cursor;
    }
    existing
}

#[cfg(target_arch = "wasm32")]
fn schedule_visible_refresh(
    mut generation: Signal<u64>,
    mut poll_ticket: Signal<u64>,
    delay_ms: i32,
) {
    let expected_ticket = poll_ticket.peek().saturating_add(1);
    poll_ticket.set(expected_ticket);
    let callback = Closure::once(move || {
        if *poll_ticket.peek() != expected_ticket {
            return;
        }
        let visible = web_sys::window()
            .and_then(|window| window.document())
            .is_none_or(|document| !document.hidden());
        if visible {
            generation.set(generation().saturating_add(1));
        } else {
            schedule_visible_refresh(generation, poll_ticket, 5_000);
        }
    });
    if let Some(window) = web_sys::window() {
        if window
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.as_ref().unchecked_ref(),
                delay_ms,
            )
            .is_ok()
        {
            callback.forget();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn schedule_visible_refresh(_generation: Signal<u64>, _poll_ticket: Signal<u64>, _delay_ms: i32) {}
