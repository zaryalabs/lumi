//! Capability-aware Community Space Web surface.

use dioxus::prelude::*;
use gloo_net::http::Request;
use lumi_core::{
    CommunityAccessLink, CommunityAccessLinkStatus, CommunityLinkPreview, CommunityMembership,
    CommunityMembershipStatus, CommunityRole, CommunitySpace, CommunitySpaceDetail,
    CreateCommunityAccessLinkRequest, CreateCommunitySpaceRequest, CreatedCommunityAccessLink,
    JoinCommunityLinkRequest, PreviewCommunityLinkRequest, UpdateCommunityMemberRequest,
    UpdateCommunitySpaceRequest,
};
use uuid::Uuid;
use web_sys::RequestCredentials;

use crate::account::{api_response_error, network_error, parse_json, ApiError, API_BASE};

#[component]
pub(crate) fn CommunityPage(
    current_user_id: Uuid,
    csrf_token: String,
    space_id: Option<Uuid>,
    join_token: Option<String>,
    available: bool,
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
    on_close: EventHandler<()>,
) -> Element {
    let csrf_token = use_signal(|| csrf_token);
    let mut detail = use_signal(|| Option::<CommunitySpaceDetail>::None);
    let mut members = use_signal(Vec::<CommunityMembership>::new);
    let mut links = use_signal(Vec::<CommunityAccessLink>::new);
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
                    article { class: "library-section",
                        h2 { "Материалы" }
                        p { "Публикация identity материалов появится в следующем социальном эпике." }
                    }
                    article { class: "library-section",
                        h2 { "Обсуждение и чат" }
                        p { "Совместное чтение и сообщения пока не включены capability-флагами." }
                    }
                    article { class: "library-section",
                        h2 { "Активность" }
                        p { "Системные события сохраняются отдельно от будущего чата." }
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
fn SpaceIdentityEditor(
    csrf_token: String,
    detail: CommunitySpaceDetail,
    on_saved: EventHandler<()>,
) -> Element {
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
                    let csrf = csrf_token.clone();
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
            if !error().is_empty() { p { class: "account-error", role: "alert", "{error}" } }
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
    let builder = match method {
        "POST" => Request::post(&format!("{API_BASE}{path}")),
        "PATCH" => Request::patch(&format!("{API_BASE}{path}")),
        _ => return Err(ApiError::Message("Неподдерживаемая операция.".to_owned())),
    };
    let request = builder
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .header("Idempotency-Key", &Uuid::now_v7().to_string())
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

fn link_status_label(status: CommunityAccessLinkStatus) -> &'static str {
    match status {
        CommunityAccessLinkStatus::Active => "Активна",
        CommunityAccessLinkStatus::Revoked => "Отозвана",
    }
}
