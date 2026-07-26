//! Unified global, Library, Reader and Desk search surfaces.

use dioxus::prelude::*;
use gloo_net::http::Request;
use lumi_core::{
    SearchIndexState, SearchOpenTarget, SearchPage, SearchResult, SearchSourceType, SearchStatus,
};
use uuid::Uuid;
use web_sys::RequestCredentials;

use crate::account::{notify_session_expired, API_BASE};
use crate::routing::{percent_encode, source_type_token, SearchRoute};

#[component]
pub(crate) fn GlobalSearchPage(
    route: SearchRoute,
    on_open: EventHandler<SearchOpenTarget>,
) -> Element {
    let mut query = use_signal(|| route.query.clone());
    let mut source_type = use_signal(|| route.source_type);
    let mut state = use_signal(SearchViewState::default);
    let route_for_load = route.clone();
    use_effect(move || {
        let route = route_for_load.clone();
        query.set(route.query.clone());
        source_type.set(route.source_type);
        state.set(SearchViewState {
            loading: !route.query.trim().is_empty(),
            ..SearchViewState::default()
        });
        spawn(async move {
            let status = load_search_status().await;
            let page = if route.query.trim().is_empty() {
                Ok(None)
            } else {
                load_search(&route).await.map(Some)
            };
            match (status, page) {
                (Ok(status), Ok(page)) => state.set(SearchViewState {
                    status: Some(status),
                    page,
                    loading: false,
                    error: None,
                }),
                (Err(error), _) | (_, Err(error)) => state.set(SearchViewState {
                    loading: false,
                    error: Some(error),
                    ..SearchViewState::default()
                }),
            }
        });
    });

    let snapshot = state.read().clone();
    let items = snapshot
        .page
        .as_ref()
        .map(|page| page.items.clone())
        .unwrap_or_default();
    rsx! {
        main { id: "main-content", class: "search-view", aria_label: "Единый поиск",
            header { class: "search-hero",
                p { class: "eyebrow", "Единый индекс" }
                h1 { "Поиск по Lumi" }
                p { "Материалы, записи, обучение и сохранённые AI-артефакты — через один permission-aware контракт." }
                form { class: "unified-search-form", role: "search", onsubmit: move |event| {
                    event.prevent_default();
                    let next = crate::routing::AppRoute::Search(SearchRoute {
                        query: query().trim().to_owned(),
                        source_type: source_type(),
                        material_id: route.material_id,
                    });
                    crate::routing::set_browser_route(&next);
                },
                    label { class: "search-query-label",
                        span { "Что найти" }
                        input {
                            r#type: "search",
                            name: "q",
                            value: "{query}",
                            placeholder: "Название, цитата, заметка или вопрос",
                            oninput: move |event| query.set(event.value()),
                        }
                    }
                    label {
                        span { "Тип" }
                        select {
                            name: "type",
                            value: source_type().map_or("", source_type_token),
                            onchange: move |event| source_type.set(parse_source_type(&event.value())),
                            option { value: "", "Все" }
                            option { value: "material", "Материалы" }
                            option { value: "note", "Заметки" }
                            option { value: "highlight", "Выделения" }
                            option { value: "learning_item", "Обучение" }
                            option { value: "ai_artifact", "AI-артефакты" }
                        }
                    }
                    button { class: "primary-action", r#type: "submit", "Найти" }
                }
            }
            if let Some(status) = snapshot.status {
                SearchStatusNotice { status }
            }
            if snapshot.loading {
                p { class: "search-state", role: "status", "Ищем по единому индексу…" }
            }
            if let Some(ref error) = snapshot.error {
                p { class: "library-alert", role: "alert", "{error}" }
            }
            if route.query.trim().is_empty() {
                section { class: "search-empty",
                    h2 { "Введите запрос" }
                    p { "Поиск не зависит от AI provider и остаётся доступным без BYOK." }
                }
            } else if !snapshot.loading && items.is_empty() && snapshot.error.is_none() {
                section { class: "search-empty",
                    h2 { "Совпадений нет" }
                    p { "Попробуйте убрать фильтр типа или уточнить формулировку." }
                }
            } else {
                SearchGroups { items, on_open }
            }
        }
    }
}

#[component]
pub(crate) fn LibrarySearch(on_submit: EventHandler<String>) -> Element {
    let mut query = use_signal(String::new);
    rsx! {
        form { class: "library-search", role: "search", aria_label: "Поиск в библиотеке", onsubmit: move |event| {
            event.prevent_default();
            let value = query().trim().to_owned();
            if !value.is_empty() {
                on_submit.call(value);
            }
        },
            label {
                span { "Найти в библиотеке и записях" }
                input {
                    r#type: "search",
                    name: "library_search",
                    placeholder: "Поиск по единому индексу",
                    value: "{query}",
                    oninput: move |event| query.set(event.value()),
                }
            }
            button { class: "secondary-action", r#type: "submit", "Поиск" }
        }
    }
}

#[component]
pub(crate) fn ReaderSearch(material_id: Uuid) -> Element {
    let mut query = use_signal(String::new);
    let mut page = use_signal(|| Option::<SearchPage>::None);
    let mut error = use_signal(String::new);
    let mut loading = use_signal(|| false);
    rsx! {
        details { class: "reader-search",
            summary { role: "button", "Поиск" }
            form { role: "search", aria_label: "Поиск в материале", onsubmit: move |event| {
                event.prevent_default();
                let value = query().trim().to_owned();
                if !value.is_empty() {
                    loading.set(true);
                    error.set(String::new());
                    spawn(async move {
                        match load_material_search(material_id, &value).await {
                            Ok(result) => page.set(Some(result)),
                            Err(message) => error.set(message),
                        }
                        loading.set(false);
                    });
                }
            },
                label {
                    span { "Поиск в материале" }
                    input {
                        r#type: "search",
                        name: "reader_search",
                        value: "{query}",
                        oninput: move |event| query.set(event.value()),
                    }
                }
                button { class: "secondary-action", r#type: "submit", "Найти" }
            }
            if loading() {
                p { role: "status", "Ищем…" }
            }
            if !error().is_empty() {
                p { class: "library-alert", role: "alert", "{error}" }
            }
            if let Some(results) = page.read().clone() {
                ol { class: "reader-search-results",
                    for result in results.items {
                        li {
                            button { r#type: "button", onclick: move |_| open_search_target(&result.open_target),
                                strong { "{result.title}" }
                                span { "{result.snippet}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SearchGroups(items: Vec<SearchResult>, on_open: EventHandler<SearchOpenTarget>) -> Element {
    let groups = [
        ("Материалы", SearchGroup::Materials),
        ("Записи", SearchGroup::Records),
        ("Обучение", SearchGroup::Learning),
        ("ИИ-артефакты", SearchGroup::Artifacts),
    ];
    rsx! {
        div { class: "search-groups", aria_live: "polite",
            for (label, group) in groups {
                if items.iter().any(|item| group.matches(item.source_type)) {
                    section { class: "search-group", aria_label: "{label}",
                        h2 { "{label}" }
                        ol {
                            for result in items.iter().filter(|item| group.matches(item.source_type)).cloned() {
                                SearchResultCard { result, on_open }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SearchResultCard(result: SearchResult, on_open: EventHandler<SearchOpenTarget>) -> Element {
    let target = result.open_target.clone();
    let path = if result.heading_path.is_empty() {
        "Без структурного пути".to_owned()
    } else {
        result.heading_path.join(" › ")
    };
    rsx! {
        li { class: "search-result",
            div {
                span { class: "status-pill ready", "{source_label(result.source_type)}" }
                h3 { "{result.title}" }
                p { class: "search-path", "{path}" }
                p { "{result.snippet}" }
                p { class: "match-reason", "Совпадение: {match_reason(result.source_type)} · score {result.score.total:.3}" }
            }
            button { class: "secondary-action", r#type: "button", onclick: move |_| on_open.call(target.clone()), "Открыть" }
        }
    }
}

#[component]
fn SearchStatusNotice(status: SearchStatus) -> Element {
    match status.state {
        SearchIndexState::Ready => rsx! {},
        SearchIndexState::Partial => rsx! {
            p { class: "search-index-notice", role: "status", "Индекс обновляется: часть новых данных может появиться позже." }
        },
        SearchIndexState::Rebuilding => rsx! {
            p { class: "search-index-notice", role: "status", "Индекс перестраивается. Уже готовые результаты остаются доступны." }
        },
        SearchIndexState::Failed => rsx! {
            p { class: "library-alert", role: "alert", "Поиск временно недоступен: модель или индекс не прошли проверку." }
        },
    }
}

#[derive(Clone, Default)]
struct SearchViewState {
    status: Option<SearchStatus>,
    page: Option<SearchPage>,
    loading: bool,
    error: Option<String>,
}

#[derive(Clone, Copy)]
enum SearchGroup {
    Materials,
    Records,
    Learning,
    Artifacts,
}

impl SearchGroup {
    fn matches(self, source_type: SearchSourceType) -> bool {
        match self {
            Self::Materials => source_type == SearchSourceType::Material,
            Self::Records => matches!(
                source_type,
                SearchSourceType::Highlight
                    | SearchSourceType::Note
                    | SearchSourceType::MarginNote
                    | SearchSourceType::VoiceTranscript
            ),
            Self::Learning => source_type == SearchSourceType::LearningItem,
            Self::Artifacts => source_type == SearchSourceType::AiArtifact,
        }
    }
}

pub(crate) fn open_search_target(target: &SearchOpenTarget) {
    let route = match target {
        SearchOpenTarget::Material { material_id } => {
            crate::routing::AppRoute::Desk(crate::routing::DeskRoute {
                view: crate::routing::DeskView::Material(*material_id),
                ..crate::routing::DeskRoute::default()
            })
        }
        SearchOpenTarget::Reader {
            material_id,
            anchor,
            ..
        } => crate::routing::AppRoute::Reader(
            *material_id,
            None,
            anchor
                .as_ref()
                .and_then(|anchor| anchor.node_path.last().cloned()),
        ),
        SearchOpenTarget::Annotation { annotation_id, .. } => {
            crate::routing::AppRoute::Desk(crate::routing::DeskRoute {
                view: crate::routing::DeskView::Item(
                    lumi_core::DeskObjectType::Annotation,
                    *annotation_id,
                ),
                ..crate::routing::DeskRoute::default()
            })
        }
        SearchOpenTarget::LearningItem { item_id } => {
            crate::routing::AppRoute::Desk(crate::routing::DeskRoute {
                view: crate::routing::DeskView::Item(
                    lumi_core::DeskObjectType::LearningItem,
                    *item_id,
                ),
                ..crate::routing::DeskRoute::default()
            })
        }
        SearchOpenTarget::AiArtifact { artifact_id } => {
            crate::routing::AppRoute::Desk(crate::routing::DeskRoute {
                view: crate::routing::DeskView::Item(
                    lumi_core::DeskObjectType::AiArtifact,
                    *artifact_id,
                ),
                ..crate::routing::DeskRoute::default()
            })
        }
    };
    crate::routing::set_browser_route(&route);
}

async fn load_search(route: &SearchRoute) -> Result<SearchPage, String> {
    let mut parameters = vec![format!("q={}", percent_encode(route.query.trim()))];
    if let Some(source_type) = route.source_type {
        parameters.push(format!("type={}", source_type_token(source_type)));
    }
    if let Some(material_id) = route.material_id {
        parameters.push("scope=material".to_owned());
        parameters.push(format!("material_id={material_id}"));
    }
    get_json(&format!("/search?{}&limit=40", parameters.join("&"))).await
}

async fn load_material_search(material_id: Uuid, query: &str) -> Result<SearchPage, String> {
    get_json(&format!(
        "/search?q={}&scope=material&material_id={material_id}&limit=20",
        percent_encode(query)
    ))
    .await
}

async fn load_search_status() -> Result<SearchStatus, String> {
    get_json("/search/status").await
}

async fn get_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
    let response = Request::get(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status() == 401 {
        notify_session_expired();
        return Err("Сессия завершена.".to_owned());
    }
    if !response.ok() {
        return Err(format!("Сервер вернул статус {}.", response.status()));
    }
    response.json().await.map_err(|error| error.to_string())
}

fn parse_source_type(value: &str) -> Option<SearchSourceType> {
    match value {
        "material" => Some(SearchSourceType::Material),
        "highlight" => Some(SearchSourceType::Highlight),
        "note" => Some(SearchSourceType::Note),
        "learning_item" => Some(SearchSourceType::LearningItem),
        "ai_artifact" => Some(SearchSourceType::AiArtifact),
        _ => None,
    }
}

fn source_label(value: SearchSourceType) -> &'static str {
    match value {
        SearchSourceType::Material => "Материал",
        SearchSourceType::Highlight => "Выделение",
        SearchSourceType::Note => "Заметка",
        SearchSourceType::MarginNote => "Запись на полях",
        SearchSourceType::VoiceTranscript => "Голос",
        SearchSourceType::AiArtifact => "AI-артефакт",
        SearchSourceType::LearningItem => "Обучение",
    }
}

fn match_reason(value: SearchSourceType) -> &'static str {
    match value {
        SearchSourceType::Material => "текст или метаданные материала",
        SearchSourceType::Highlight => "текст выделения",
        SearchSourceType::Note | SearchSourceType::MarginNote => "текст или тег записи",
        SearchSourceType::VoiceTranscript => "принятый транскрипт",
        SearchSourceType::AiArtifact => "сохранённый артефакт",
        SearchSourceType::LearningItem => "активный учебный вопрос",
    }
}
