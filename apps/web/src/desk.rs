//! Material-centred Desk surface over the rebuildable primary-backed projection.

use dioxus::prelude::*;
use gloo_net::http::Request;
use lumi_core::{
    Annotation, AnnotationBacklink, AnnotationKind, DeskItem, DeskItemPage, DeskLearningState,
    DeskMaterialPage, DeskObjectType, MaterialDesk, RecordSearchScope, SearchOpenTarget,
    SearchSourceType, UpdateAnnotationCommand, RECORD_RETRIEVAL_VERSION,
};
use uuid::Uuid;
use web_sys::RequestCredentials;

use crate::account::{notify_session_expired, API_BASE};
use crate::routing::{percent_encode, AppRoute, DeskRoute, DeskView, SearchRoute};

#[component]
pub(crate) fn DeskPage(
    route: DeskRoute,
    csrf_token: String,
    on_route: EventHandler<AppRoute>,
) -> Element {
    let mut state = use_signal(|| DeskState::Loading);
    let mut search_query = use_signal(String::new);
    let route_for_load = route.clone();
    use_effect(move || {
        state.set(DeskState::Loading);
        let route = route_for_load.clone();
        spawn(async move {
            let loaded = match route.view {
                DeskView::Overview => load_materials().await.map(DeskState::Materials),
                DeskView::Material(material_id) => load_material(material_id)
                    .await
                    .map(|material| DeskState::Material(Box::new(material))),
                DeskView::Records(material_id) => {
                    load_items(&route, material_id, Some(DeskObjectType::Annotation))
                        .await
                        .map(DeskState::Items)
                }
                DeskView::Learning(material_id) => {
                    load_items(&route, material_id, Some(DeskObjectType::LearningItem))
                        .await
                        .map(DeskState::Items)
                }
                DeskView::Artifacts(material_id) => {
                    load_items(&route, material_id, Some(DeskObjectType::AiArtifact))
                        .await
                        .map(DeskState::Items)
                }
                DeskView::Item(object_type, object_id) => {
                    load_item(object_type, object_id).await.map(DeskState::Item)
                }
            };
            state.set(loaded.unwrap_or_else(DeskState::Failed));
        });
    });

    let selected_view = route.view.clone();
    let ask_material = material_from_view(&route.view);
    let ask_types = match &route.view {
        DeskView::Learning(_) => vec![SearchSourceType::LearningItem],
        DeskView::Artifacts(_) => vec![SearchSourceType::AiArtifact],
        DeskView::Records(_) | DeskView::Item(DeskObjectType::Annotation, _) => vec![
            SearchSourceType::Highlight,
            SearchSourceType::Note,
            SearchSourceType::MarginNote,
            SearchSourceType::VoiceTranscript,
        ],
        _ => Vec::new(),
    };
    let ask_scope = RecordSearchScope {
        query: None,
        material_ids: ask_material.into_iter().collect(),
        record_types: ask_types,
        tags: route.tag.clone().into_iter().collect(),
        statuses: vec!["active".to_owned()],
        updated_range: None,
        retrieval_version: RECORD_RETRIEVAL_VERSION.to_owned(),
    };
    let ask_label = ask_material.map_or_else(
        || "Все личные записи".to_owned(),
        |_| "Записи текущего материала".to_owned(),
    );
    rsx! {
        main { id: "main-content", class: "desk-view", aria_label: "Desk",
            header { class: "desk-hero",
                div {
                    p { class: "eyebrow", "Рабочий стол чтения" }
                    h1 { "Desk" }
                    p { "Записи, обучение и сохранённые результаты собраны вокруг исходных материалов." }
                }
                form { class: "desk-search", role: "search", onsubmit: move |event| {
                    event.prevent_default();
                    let query = search_query();
                    if !query.trim().is_empty() {
                        on_route.call(AppRoute::Search(SearchRoute {
                            query: query.trim().to_owned(),
                            source_type: match selected_view {
                                DeskView::Records(_) => Some(lumi_core::SearchSourceType::Note),
                                DeskView::Learning(_) => Some(lumi_core::SearchSourceType::LearningItem),
                                DeskView::Artifacts(_) => Some(lumi_core::SearchSourceType::AiArtifact),
                                _ => None,
                            },
                            material_id: material_from_view(&selected_view),
                        }));
                    }
                },
                    label {
                        span { "Поиск в Desk" }
                        input {
                            r#type: "search",
                            name: "desk_search",
                            placeholder: "Найти запись или вопрос",
                            value: "{search_query}",
                            oninput: move |event| search_query.set(event.value()),
                        }
                    }
                    button { class: "secondary-action", r#type: "submit", "Найти" }
                }
                button {
                    class: "primary-action",
                    r#type: "button",
                    onclick: move |_| {
                        let _ = crate::ai::dispatch_record_handoff(
                            ask_scope.clone(),
                            ask_label.clone(),
                        );
                    },
                    "Спросить по записям"
                }
            }
            nav { class: "desk-tabs", aria_label: "Разделы Desk",
                DeskNavLink { label: "Обзор", target: DeskView::Overview, current: route.view.clone(), on_route }
                DeskNavLink { label: "Все записи", target: DeskView::Records(None), current: route.view.clone(), on_route }
                DeskNavLink { label: "Обучение", target: DeskView::Learning(None), current: route.view.clone(), on_route }
                DeskNavLink { label: "AI-артефакты", target: DeskView::Artifacts(None), current: route.view.clone(), on_route }
            }
            DeskFilters { route: route.clone(), on_route }
            match state.read().clone() {
                DeskState::Loading => rsx! {
                    p { class: "desk-state", role: "status", "Обновляем Desk…" }
                },
                DeskState::Failed(error) => rsx! {
                    p { class: "library-alert", role: "alert", "{error}" }
                },
                DeskState::Materials(page) => rsx! {
                    DeskMaterialGrid { page, on_route }
                },
                DeskState::Material(material) => rsx! {
                    MaterialOverview { material: *material, on_route }
                },
                DeskState::Items(page) => rsx! {
                    DeskItemList { page, on_route }
                },
                DeskState::Item(item) => rsx! {
                    DeskItemDetail {
                        item,
                        csrf_token: csrf_token.clone(),
                        on_route,
                        on_saved: move |updated| state.set(DeskState::Item(updated)),
                    }
                },
            }
        }
    }
}

#[component]
fn DeskNavLink(
    label: &'static str,
    target: DeskView,
    current: DeskView,
    on_route: EventHandler<AppRoute>,
) -> Element {
    let active = same_section(&target, &current);
    rsx! {
        button {
            class: if active { "active" } else { "" },
            r#type: "button",
            aria_current: if active { "page" } else { "false" },
            onclick: move |_| on_route.call(AppRoute::Desk(DeskRoute {
                view: target.clone(),
                ..DeskRoute::default()
            })),
            "{label}"
        }
    }
}

#[component]
fn DeskFilters(route: DeskRoute, on_route: EventHandler<AppRoute>) -> Element {
    if matches!(
        route.view,
        DeskView::Overview | DeskView::Material(_) | DeskView::Item(_, _)
    ) {
        return rsx! {};
    }
    let current_route = route.clone();
    let route_for_status = route.clone();
    rsx! {
        div { class: "desk-filters", role: "group", aria_label: "Фильтры Desk",
            label {
                span { "Сортировка" }
                select {
                    value: route.sort.as_deref().unwrap_or("updated"),
                    onchange: move |event| {
                        let mut next = current_route.clone();
                        next.sort = Some(event.value());
                        on_route.call(AppRoute::Desk(next));
                    },
                    option { value: "updated", "Недавно изменённые" }
                    option { value: "created", "Недавно созданные" }
                    option { value: "source_order", "По источнику" }
                    option { value: "material_title", "По материалу" }
                }
            }
            if matches!(route.view, DeskView::Learning(_)) {
                label {
                    span { "Состояние" }
                    select {
                        value: route.status.as_deref().unwrap_or(""),
                        onchange: move |event| {
                            let mut next = route_for_status.clone();
                            next.status = non_empty(event.value());
                            on_route.call(AppRoute::Desk(next));
                        },
                        option { value: "", "Все" }
                        option { value: "due", "Сегодня" }
                        option { value: "missed", "Пропущено" }
                        option { value: "skipped", "Отложено" }
                        option { value: "completed", "Пройдено" }
                    }
                }
            }
        }
    }
}

#[component]
fn DeskMaterialGrid(page: DeskMaterialPage, on_route: EventHandler<AppRoute>) -> Element {
    if page.items.is_empty() {
        return rsx! {
            section { class: "desk-empty",
                h2 { "Desk пока пуст" }
                p { "Добавьте материал или запись — проекция появится автоматически." }
            }
        };
    }
    rsx! {
        section { aria_label: "Материалы в Desk",
            div { class: "section-heading",
                div { p { class: "eyebrow", "Материалы" } h2 { "Работа по материалам" } }
                span { "Проекция {page.projection_generation}" }
            }
            div { class: "desk-material-grid",
                for material in page.items {
                    article { class: "desk-material-card",
                        h3 { "{material.title}" }
                        dl {
                            div { dt { "Записи" } dd { "{material.record_count}" } }
                            div { dt { "Обучение" } dd { "{material.learning_count}" } }
                            div { dt { "Артефакты" } dd { "{material.artifact_count}" } }
                        }
                        if material.attention_count > 0 {
                            p { class: "attention-note", "{material.attention_count} требуют внимания" }
                        }
                        button { class: "primary-action", r#type: "button", onclick: move |_| {
                            on_route.call(AppRoute::Desk(DeskRoute {
                                view: DeskView::Material(material.material_id),
                                ..DeskRoute::default()
                            }));
                        }, "Открыть материал" }
                    }
                }
            }
        }
    }
}

#[component]
fn MaterialOverview(material: MaterialDesk, on_route: EventHandler<AppRoute>) -> Element {
    let material_id = material.material.material_id;
    rsx! {
        section { class: "material-desk",
            div { class: "section-heading",
                div {
                    p { class: "eyebrow", "Desk материала" }
                    h2 { "{material.material.title}" }
                }
                span { "{material.material.record_count + material.material.learning_count + material.material.artifact_count} элементов" }
            }
            div { class: "desk-overview-actions",
                button { class: "secondary-action", r#type: "button", onclick: move |_| on_route.call(AppRoute::Desk(DeskRoute { view: DeskView::Records(Some(material_id)), ..DeskRoute::default() })), "Записи ({material.material.record_count})" }
                button { class: "secondary-action", r#type: "button", onclick: move |_| on_route.call(AppRoute::Desk(DeskRoute { view: DeskView::Learning(Some(material_id)), ..DeskRoute::default() })), "Обучение ({material.material.learning_count})" }
                button { class: "secondary-action", r#type: "button", onclick: move |_| on_route.call(AppRoute::Desk(DeskRoute { view: DeskView::Artifacts(Some(material_id)), ..DeskRoute::default() })), "Артефакты ({material.material.artifact_count})" }
                button { class: "primary-action", r#type: "button", onclick: move |_| on_route.call(AppRoute::Reader(material_id, None, None)), "Открыть Reader" }
            }
            DeskItemList { page: material.items, on_route }
        }
    }
}

#[component]
fn DeskItemList(page: DeskItemPage, on_route: EventHandler<AppRoute>) -> Element {
    if page.items.is_empty() {
        return rsx! {
            section { class: "desk-empty", h2 { "Здесь пока нет элементов" } p { "Измените фильтры или создайте запись в Reader." } }
        };
    }
    rsx! {
        ol { class: "desk-item-list", aria_live: "polite",
            for item in page.items {
                li {
                    article { class: "desk-item-card",
                        div {
                            span { class: "status-pill ready", "{desk_type_label(item.object_type)}" }
                            span { class: "desk-material-name", "{item.material_title}" }
                            h3 { "{item.title}" }
                            p { "{item.preview}" }
                            if !item.structural_path.is_empty() {
                                p { class: "search-path", "{item.structural_path.join(\" › \")}" }
                            }
                            if item.attention {
                                p { class: "attention-note", "Требует внимания" }
                            }
                            if let Some(state) = item.learning_state {
                                p { class: "match-reason", "{learning_state_label(state)} · попыток {item.attempt_count}" }
                            }
                        }
                        button { class: "secondary-action", r#type: "button", onclick: move |_| {
                            on_route.call(AppRoute::Desk(DeskRoute {
                                view: DeskView::Item(item.object_type, item.object_id),
                                ..DeskRoute::default()
                            }));
                        }, "Открыть" }
                    }
                }
            }
        }
    }
}

#[component]
fn DeskItemDetail(
    item: DeskItem,
    csrf_token: String,
    on_route: EventHandler<AppRoute>,
    on_saved: EventHandler<DeskItem>,
) -> Element {
    let mut annotation = use_signal(|| Option::<Annotation>::None);
    let mut draft = use_signal(|| item.preview.clone());
    let mut title = use_signal(|| item.title.clone());
    let mut tags = use_signal(|| item.tags.join(", "));
    let mut backlinks = use_signal(Vec::<AnnotationBacklink>::new);
    let mut message = use_signal(String::new);
    use_effect(move || {
        if item.object_type == DeskObjectType::Annotation {
            spawn(async move {
                match load_annotation(item.material_id, item.object_id).await {
                    Ok(value) => {
                        draft.set(value.note_body().unwrap_or_default().to_owned());
                        title.set(value.title.clone().unwrap_or_default());
                        tags.set(value.tags.join(", "));
                        annotation.set(Some(value));
                    }
                    Err(error) => message.set(error),
                }
                if let Ok(items) = get_json::<Vec<AnnotationBacklink>>(&format!(
                    "/links/backlinks/annotation/{}",
                    item.object_id
                ))
                .await
                {
                    backlinks.set(items);
                }
            });
        }
    });
    let source_anchor = item.structural_path.last().cloned();
    rsx! {
        article { class: "desk-detail",
            p { class: "eyebrow", "{desk_type_label(item.object_type)} · {item.material_title}" }
            h2 { "{item.title}" }
            if item.object_type == DeskObjectType::Annotation {
                if let Some(current) = annotation.read().clone() {
                    if matches!(current.kind, AnnotationKind::Note { .. }) {
                        form { class: "desk-inline-editor", onsubmit: move |event| {
                            event.prevent_default();
                            let csrf = csrf_token.clone();
                            let body = draft().trim().to_owned();
                            let next_title = non_empty(title());
                            let next_tags = parse_tags(&tags());
                            let previous = current.clone();
                            message.set("Сохраняем…".to_owned());
                            spawn(async move {
                                let command = UpdateAnnotationCommand {
                                    material_id: previous.material_id,
                                    annotation_id: previous.id,
                                    expected_revision: previous.revision,
                                    target: previous.target,
                                    kind: AnnotationKind::Note { body },
                                    title: next_title,
                                    tags: next_tags,
                                    status: previous.status,
                                    related_annotation_id: previous.related_annotation_id,
                                };
                                match update_annotation(&command, &csrf).await {
                                    Ok(updated) => {
                                        annotation.set(Some(updated));
                                        message.set("Сохранено".to_owned());
                                        if let Ok(projected) = load_item(DeskObjectType::Annotation, previous.id).await {
                                            on_saved.call(projected);
                                        }
                                    }
                                    Err(error) => message.set(error),
                                }
                            });
                        },
                            label { "Заголовок", input { value: "{title}", oninput: move |event| title.set(event.value()) } }
                            label { "Текст заметки", textarea { rows: "10", value: "{draft}", oninput: move |event| draft.set(event.value()) } }
                            label { "Теги", input { value: "{tags}", oninput: move |event| tags.set(event.value()) } }
                            button { class: "primary-action", r#type: "submit", disabled: draft().trim().is_empty(), "Сохранить" }
                        }
                    } else {
                        p { "{item.preview}" }
                    }
                } else {
                    p { role: "status", "Загружаем исходную запись…" }
                }
            } else {
                p { "{item.preview}" }
            }
            if !message().is_empty() {
                p { class: "save-message", role: "status", "{message}" }
            }
            div { class: "desk-detail-actions",
                button { class: "primary-action", r#type: "button", onclick: move |_| on_route.call(AppRoute::Reader(item.material_id, None, source_anchor.clone())), "Открыть источник" }
                if let Some(related_id) = annotation.read().as_ref().and_then(|value| value.related_annotation_id) {
                    button { class: "secondary-action", r#type: "button", onclick: move |_| on_route.call(AppRoute::Desk(DeskRoute {
                        view: DeskView::Item(DeskObjectType::Annotation, related_id),
                        ..DeskRoute::default()
                    })), "Открыть связанную запись" }
                }
                if item.object_type == DeskObjectType::LearningItem {
                    button { class: "secondary-action", r#type: "button", onclick: move |_| on_route.call(AppRoute::Challenges), "Открыть Челленджи" }
                }
                button { class: "secondary-action", r#type: "button", onclick: move |_| on_route.call(AppRoute::Desk(DeskRoute { view: DeskView::Material(item.material_id), ..DeskRoute::default() })), "К материалу" }
            }
            if !backlinks.read().is_empty() {
                section { class: "desk-backlinks", aria_label: "Обратные ссылки",
                    h3 { "Обратные ссылки" }
                    ul {
                        for backlink in backlinks.read().iter().cloned() {
                            li {
                                button { class: "secondary-action", r#type: "button", onclick: move |_| on_route.call(AppRoute::Desk(DeskRoute {
                                    view: DeskView::Item(DeskObjectType::Annotation, backlink.source_annotation_id),
                                    ..DeskRoute::default()
                                })),
                                    "{backlink.source_display_path}"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone)]
enum DeskState {
    Loading,
    Failed(String),
    Materials(DeskMaterialPage),
    Material(Box<MaterialDesk>),
    Items(DeskItemPage),
    Item(DeskItem),
}

async fn load_materials() -> Result<DeskMaterialPage, String> {
    get_json("/desk/materials?limit=60").await
}

async fn load_material(material_id: Uuid) -> Result<MaterialDesk, String> {
    get_json(&format!("/desk/materials/{material_id}")).await
}

async fn load_items(
    route: &DeskRoute,
    material_id: Option<Uuid>,
    object_type: Option<DeskObjectType>,
) -> Result<DeskItemPage, String> {
    let mut parameters = vec!["limit=60".to_owned()];
    if let Some(material_id) = material_id {
        parameters.push(format!("material_id={material_id}"));
    }
    if let Some(object_type) = object_type {
        parameters.push(format!("type={}", object_type.as_str()));
    }
    if let Some(tag) = &route.tag {
        parameters.push(format!("tag={}", percent_encode(tag)));
    }
    if let Some(status) = &route.status {
        let key = if object_type == Some(DeskObjectType::LearningItem) {
            "learning_state"
        } else {
            "status"
        };
        parameters.push(format!("{key}={}", percent_encode(status)));
    }
    if let Some(sort) = &route.sort {
        parameters.push(format!("sort={}", percent_encode(sort)));
    }
    get_json(&format!("/desk/items?{}", parameters.join("&"))).await
}

async fn load_item(object_type: DeskObjectType, object_id: Uuid) -> Result<DeskItem, String> {
    get_json(&format!("/desk/items/{}/{object_id}", object_type.as_str())).await
}

async fn load_annotation(material_id: Uuid, annotation_id: Uuid) -> Result<Annotation, String> {
    get_json(&format!(
        "/materials/{material_id}/annotations/{annotation_id}"
    ))
    .await
}

async fn update_annotation(
    command: &UpdateAnnotationCommand,
    csrf: &str,
) -> Result<Annotation, String> {
    let response = Request::put(&format!(
        "{API_BASE}/materials/{}/annotations/{}",
        command.material_id, command.annotation_id
    ))
    .credentials(RequestCredentials::Include)
    .header("content-type", "application/json")
    .header("x-lumi-csrf", csrf)
    .header("Idempotency-Key", &Uuid::now_v7().to_string())
    .body(serde_json::to_string(command).map_err(|error| error.to_string())?)
    .map_err(|error| error.to_string())?
    .send()
    .await
    .map_err(|error| error.to_string())?;
    response_json(response).await
}

async fn get_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
    let response = Request::get(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    response_json(response).await
}

async fn response_json<T: serde::de::DeserializeOwned>(
    response: gloo_net::http::Response,
) -> Result<T, String> {
    if response.status() == 401 {
        notify_session_expired();
        return Err("Сессия завершена.".to_owned());
    }
    if response.status() == 409 {
        return Err("Запись уже изменилась в Reader. Обновите Desk и повторите.".to_owned());
    }
    if !response.ok() {
        return Err(format!("Сервер вернул статус {}.", response.status()));
    }
    response.json().await.map_err(|error| error.to_string())
}

fn same_section(left: &DeskView, right: &DeskView) -> bool {
    matches!(
        (left, right),
        (DeskView::Overview, DeskView::Overview)
            | (DeskView::Material(_), DeskView::Material(_))
            | (DeskView::Records(_), DeskView::Records(_))
            | (DeskView::Learning(_), DeskView::Learning(_))
            | (DeskView::Artifacts(_), DeskView::Artifacts(_))
            | (DeskView::Item(_, _), DeskView::Item(_, _))
    )
}

fn material_from_view(view: &DeskView) -> Option<Uuid> {
    match view {
        DeskView::Material(material_id)
        | DeskView::Records(Some(material_id))
        | DeskView::Learning(Some(material_id))
        | DeskView::Artifacts(Some(material_id)) => Some(*material_id),
        _ => None,
    }
}

fn desk_type_label(value: DeskObjectType) -> &'static str {
    match value {
        DeskObjectType::Annotation => "Запись",
        DeskObjectType::LearningItem => "Обучение",
        DeskObjectType::AiArtifact => "AI-артефакт",
    }
}

fn learning_state_label(value: DeskLearningState) -> &'static str {
    match value {
        DeskLearningState::Scheduled => "Запланировано",
        DeskLearningState::Due => "Сегодня",
        DeskLearningState::Missed => "Пропущено",
        DeskLearningState::Skipped => "Отложено",
        DeskLearningState::Completed => "Пройдено",
    }
}

fn parse_tags(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect()
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

#[allow(dead_code)]
fn open_target_route(target: &SearchOpenTarget) -> AppRoute {
    match target {
        SearchOpenTarget::Material { material_id } => AppRoute::Desk(DeskRoute {
            view: DeskView::Material(*material_id),
            ..DeskRoute::default()
        }),
        SearchOpenTarget::Reader {
            material_id,
            anchor,
            ..
        } => AppRoute::Reader(
            *material_id,
            None,
            anchor
                .as_ref()
                .and_then(|anchor| anchor.node_path.last().cloned()),
        ),
        SearchOpenTarget::Annotation { annotation_id, .. } => AppRoute::Desk(DeskRoute {
            view: DeskView::Item(DeskObjectType::Annotation, *annotation_id),
            ..DeskRoute::default()
        }),
        SearchOpenTarget::LearningItem { item_id } => AppRoute::Desk(DeskRoute {
            view: DeskView::Item(DeskObjectType::LearningItem, *item_id),
            ..DeskRoute::default()
        }),
        SearchOpenTarget::AiArtifact { artifact_id } => AppRoute::Desk(DeskRoute {
            view: DeskView::Item(DeskObjectType::AiArtifact, *artifact_id),
            ..DeskRoute::default()
        }),
    }
}
