//! Typed, reload-safe hash routing for the Web application.

use lumi_core::{Anchor, DeskObjectType, SearchSourceType};
use uuid::Uuid;

/// Top-level Web route.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AppRoute {
    Library,
    Community,
    CommunitySpace(Uuid, Option<CommunityTarget>),
    CommunityJoin,
    Challenges,
    AiQueue,
    Connections,
    Settings,
    Admin,
    Reader(Uuid, Option<ReaderOrigin>, Option<Box<Anchor>>),
    LearningSession(Uuid, LearningOrigin),
    MaterialLearning(Uuid, Option<Uuid>),
    Desk(DeskRoute),
    Search(SearchRoute),
}

/// User-visible destination restored after leaving Reader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReaderOrigin {
    Library,
    LearningSession {
        session_id: Uuid,
        parent: LearningOrigin,
    },
    CommunitySpace(Uuid),
}

/// User-visible destination restored after finishing a learning session.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum LearningOrigin {
    #[default]
    Library,
    Challenges,
    Material {
        material_id: Uuid,
        source_id: Option<Uuid>,
    },
}

/// Exact social identity preserved by Community search routes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommunityTarget {
    Material {
        shared_material_id: Uuid,
        source_id: Option<Uuid>,
    },
    Chat {
        message_id: Uuid,
    },
}

/// Typed Desk view and selected object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeskRoute {
    pub(crate) view: DeskView,
    pub(crate) tag: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) sort: Option<String>,
}

impl Default for DeskRoute {
    fn default() -> Self {
        Self {
            view: DeskView::Overview,
            tag: None,
            status: None,
            sort: None,
        }
    }
}

/// Direct Desk destination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DeskView {
    Overview,
    Records(Option<Uuid>),
    Learning(Option<Uuid>),
    Artifacts(Option<Uuid>),
    Material(Uuid),
    Item(DeskObjectType, Uuid),
}

/// Restorable global-search state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SearchRoute {
    pub(crate) query: String,
    pub(crate) source_type: Option<SearchSourceType>,
    pub(crate) material_id: Option<Uuid>,
}

pub(crate) fn initial_route() -> AppRoute {
    let hash = browser_hash();
    parse_hash(&hash).unwrap_or(AppRoute::Library)
}

pub(crate) fn set_browser_route(route: &AppRoute) {
    if let Some(window) = web_sys::window() {
        let _ = window.location().set_hash(&route_hash(route));
    }
}

pub(crate) fn browser_requests_system_settings() -> bool {
    browser_hash() == "#admin"
}

fn browser_hash() -> String {
    web_sys::window()
        .and_then(|window| window.location().hash().ok())
        .unwrap_or_default()
}

fn parse_hash(hash: &str) -> Option<AppRoute> {
    let value = hash.strip_prefix('#').unwrap_or(hash);
    let (path, query) = value
        .split_once('?')
        .map_or((value, ""), |(path, query)| (path, query));
    match path {
        "" | "library" => Some(AppRoute::Library),
        "settings" => Some(AppRoute::Settings),
        "admin" => Some(AppRoute::Admin),
        "communities" => Some(AppRoute::Community),
        "challenges" => Some(AppRoute::Challenges),
        "connections" => Some(AppRoute::Connections),
        "ai-queue" => Some(AppRoute::AiQueue),
        "desk" => Some(AppRoute::Desk(DeskRoute::default())),
        "desk/records" => Some(AppRoute::Desk(DeskRoute {
            view: DeskView::Records(None),
            ..desk_filters(query)
        })),
        "desk/learning" => Some(AppRoute::Desk(DeskRoute {
            view: DeskView::Learning(None),
            ..desk_filters(query)
        })),
        "desk/artifacts" => Some(AppRoute::Desk(DeskRoute {
            view: DeskView::Artifacts(None),
            ..desk_filters(query)
        })),
        "search" => Some(AppRoute::Search(parse_search(query))),
        _ => parse_dynamic_route(path, query),
    }
}

fn parse_dynamic_route(path: &str, query: &str) -> Option<AppRoute> {
    if path
        .strip_prefix("join/")
        .is_some_and(|token| !token.is_empty())
    {
        return Some(AppRoute::CommunityJoin);
    }
    if let Some(id) = path.strip_prefix("community/") {
        return Uuid::parse_str(id)
            .ok()
            .map(|space_id| AppRoute::CommunitySpace(space_id, parse_community_target(query)));
    }
    if let Some(id) = path.strip_prefix("learn/session/") {
        return Uuid::parse_str(id)
            .ok()
            .map(|session_id| AppRoute::LearningSession(session_id, parse_learning_origin(query)));
    }
    if let Some(id) = path.strip_prefix("reader/") {
        return Uuid::parse_str(id).ok().map(|material_id| {
            AppRoute::Reader(
                material_id,
                parse_reader_origin(query),
                query_value(query, "anchor")
                    .and_then(|value| serde_json::from_str::<Anchor>(&value).ok().map(Box::new)),
            )
        });
    }
    if let Some(id) = path
        .strip_prefix("material/")
        .and_then(|value| value.strip_suffix("/learning"))
    {
        return Uuid::parse_str(id).ok().map(|material_id| {
            AppRoute::MaterialLearning(
                material_id,
                query_value(query, "source").and_then(|id| Uuid::parse_str(&id).ok()),
            )
        });
    }
    if let Some(id) = path.strip_prefix("desk/material/") {
        return Uuid::parse_str(id).ok().map(|material_id| {
            let view = match query_value(query, "view").as_deref() {
                Some("records") => DeskView::Records(Some(material_id)),
                Some("learning") => DeskView::Learning(Some(material_id)),
                Some("artifacts") => DeskView::Artifacts(Some(material_id)),
                _ => DeskView::Material(material_id),
            };
            AppRoute::Desk(DeskRoute {
                view,
                ..desk_filters(query)
            })
        });
    }
    if let Some(value) = path.strip_prefix("desk/item/") {
        let mut parts = value.split('/');
        let object_type = parse_desk_object_type(parts.next()?)?;
        let object_id = Uuid::parse_str(parts.next()?).ok()?;
        if parts.next().is_some() {
            return None;
        }
        return Some(AppRoute::Desk(DeskRoute {
            view: DeskView::Item(object_type, object_id),
            ..desk_filters(query)
        }));
    }
    None
}

fn route_hash(route: &AppRoute) -> String {
    match route {
        AppRoute::Library => "library".to_owned(),
        AppRoute::Community => "communities".to_owned(),
        AppRoute::CommunitySpace(space_id, target) => {
            let mut parameters = Vec::new();
            match target {
                Some(CommunityTarget::Material {
                    shared_material_id,
                    source_id,
                }) => {
                    parameters.push("target=material".to_owned());
                    parameters.push(format!("shared_material_id={shared_material_id}"));
                    if let Some(source_id) = source_id {
                        parameters.push(format!("source_id={source_id}"));
                    }
                }
                Some(CommunityTarget::Chat { message_id }) => {
                    parameters.push("target=chat".to_owned());
                    parameters.push(format!("message_id={message_id}"));
                }
                None => {}
            }
            with_query(format!("community/{space_id}"), parameters)
        }
        AppRoute::CommunityJoin => "join".to_owned(),
        AppRoute::Challenges => "challenges".to_owned(),
        AppRoute::AiQueue => "ai-queue".to_owned(),
        AppRoute::Connections => "connections".to_owned(),
        AppRoute::Settings => "settings".to_owned(),
        AppRoute::Admin => "admin".to_owned(),
        AppRoute::Reader(material_id, origin, anchor) => {
            let mut parameters = Vec::new();
            if let Some(origin) = origin {
                push_reader_origin(&mut parameters, *origin);
            }
            if let Some(anchor) = anchor {
                if let Ok(serialized) = serde_json::to_string(anchor) {
                    parameters.push(format!("anchor={}", percent_encode(&serialized)));
                }
            }
            with_query(format!("reader/{material_id}"), parameters)
        }
        AppRoute::LearningSession(session_id, origin) => {
            let mut parameters = Vec::new();
            push_learning_origin(&mut parameters, *origin, "origin");
            with_query(format!("learn/session/{session_id}"), parameters)
        }
        AppRoute::MaterialLearning(material_id, source_id) => source_id.map_or_else(
            || format!("material/{material_id}/learning"),
            |source_id| format!("material/{material_id}/learning?source={source_id}"),
        ),
        AppRoute::Desk(route) => desk_hash(route),
        AppRoute::Search(route) => {
            let mut parameters = Vec::new();
            if !route.query.is_empty() {
                parameters.push(format!("q={}", percent_encode(&route.query)));
            }
            if let Some(source_type) = route.source_type {
                parameters.push(format!("type={}", source_type_token(source_type)));
            }
            if let Some(material_id) = route.material_id {
                parameters.push(format!("material_id={material_id}"));
            }
            with_query("search".to_owned(), parameters)
        }
    }
}

fn parse_reader_origin(query: &str) -> Option<ReaderOrigin> {
    match query_value(query, "origin").as_deref() {
        Some("library") => Some(ReaderOrigin::Library),
        Some("learning") => {
            let session_id =
                query_value(query, "session").and_then(|value| Uuid::parse_str(&value).ok())?;
            Some(ReaderOrigin::LearningSession {
                session_id,
                parent: parse_learning_origin_with_prefix(query, "parent"),
            })
        }
        Some("community") => query_value(query, "space")
            .and_then(|value| Uuid::parse_str(&value).ok())
            .map(ReaderOrigin::CommunitySpace),
        _ => query_value(query, "return_to")
            .and_then(|value| Uuid::parse_str(&value).ok())
            .map(|session_id| ReaderOrigin::LearningSession {
                session_id,
                parent: LearningOrigin::Library,
            }),
    }
}

fn push_reader_origin(parameters: &mut Vec<String>, origin: ReaderOrigin) {
    match origin {
        ReaderOrigin::Library => parameters.push("origin=library".to_owned()),
        ReaderOrigin::LearningSession { session_id, parent } => {
            parameters.push("origin=learning".to_owned());
            parameters.push(format!("session={session_id}"));
            push_learning_origin(parameters, parent, "parent");
        }
        ReaderOrigin::CommunitySpace(space_id) => {
            parameters.push("origin=community".to_owned());
            parameters.push(format!("space={space_id}"));
        }
    }
}

fn parse_learning_origin(query: &str) -> LearningOrigin {
    parse_learning_origin_with_prefix(query, "origin")
}

fn parse_learning_origin_with_prefix(query: &str, prefix: &str) -> LearningOrigin {
    match query_value(query, prefix).as_deref() {
        Some("challenges") => LearningOrigin::Challenges,
        Some("material") => query_value(query, &format!("{prefix}_material"))
            .and_then(|value| Uuid::parse_str(&value).ok())
            .map_or(LearningOrigin::Library, |material_id| {
                LearningOrigin::Material {
                    material_id,
                    source_id: query_value(query, &format!("{prefix}_source"))
                        .and_then(|value| Uuid::parse_str(&value).ok()),
                }
            }),
        _ => LearningOrigin::Library,
    }
}

fn push_learning_origin(parameters: &mut Vec<String>, origin: LearningOrigin, prefix: &str) {
    match origin {
        LearningOrigin::Library => parameters.push(format!("{prefix}=library")),
        LearningOrigin::Challenges => parameters.push(format!("{prefix}=challenges")),
        LearningOrigin::Material {
            material_id,
            source_id,
        } => {
            parameters.push(format!("{prefix}=material"));
            parameters.push(format!("{prefix}_material={material_id}"));
            if let Some(source_id) = source_id {
                parameters.push(format!("{prefix}_source={source_id}"));
            }
        }
    }
}

fn parse_community_target(query: &str) -> Option<CommunityTarget> {
    match query_value(query, "target").as_deref() {
        Some("material") => Some(CommunityTarget::Material {
            shared_material_id: query_value(query, "shared_material_id")
                .and_then(|value| Uuid::parse_str(&value).ok())?,
            source_id: query_value(query, "source_id")
                .and_then(|value| Uuid::parse_str(&value).ok()),
        }),
        Some("chat") => query_value(query, "message_id")
            .and_then(|value| Uuid::parse_str(&value).ok())
            .map(|message_id| CommunityTarget::Chat { message_id }),
        _ => None,
    }
}

fn desk_hash(route: &DeskRoute) -> String {
    let path = match route.view {
        DeskView::Overview => "desk".to_owned(),
        DeskView::Records(None) => "desk/records".to_owned(),
        DeskView::Learning(None) => "desk/learning".to_owned(),
        DeskView::Artifacts(None) => "desk/artifacts".to_owned(),
        DeskView::Material(material_id) => format!("desk/material/{material_id}"),
        DeskView::Records(Some(material_id)) => {
            format!("desk/material/{material_id}?view=records")
        }
        DeskView::Learning(Some(material_id)) => {
            format!("desk/material/{material_id}?view=learning")
        }
        DeskView::Artifacts(Some(material_id)) => {
            format!("desk/material/{material_id}?view=artifacts")
        }
        DeskView::Item(object_type, object_id) => {
            format!(
                "desk/item/{}/{object_id}",
                desk_object_type_token(object_type)
            )
        }
    };
    let separator = if path.contains('?') { '&' } else { '?' };
    let mut parameters = Vec::new();
    if let Some(tag) = &route.tag {
        parameters.push(format!("tag={}", percent_encode(tag)));
    }
    if let Some(status) = &route.status {
        parameters.push(format!("status={}", percent_encode(status)));
    }
    if let Some(sort) = &route.sort {
        parameters.push(format!("sort={}", percent_encode(sort)));
    }
    if parameters.is_empty() {
        path
    } else {
        format!("{path}{separator}{}", parameters.join("&"))
    }
}

fn desk_filters(query: &str) -> DeskRoute {
    DeskRoute {
        view: DeskView::Overview,
        tag: query_value(query, "tag"),
        status: query_value(query, "status"),
        sort: query_value(query, "sort"),
    }
}

fn parse_search(query: &str) -> SearchRoute {
    SearchRoute {
        query: query_value(query, "q").unwrap_or_default(),
        source_type: query_value(query, "type")
            .as_deref()
            .and_then(parse_search_source_type),
        material_id: query_value(query, "material_id")
            .and_then(|value| Uuid::parse_str(&value).ok()),
    }
}

fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|part| {
        let (name, value) = part.split_once('=')?;
        (name == key).then(|| percent_decode(value))
    })
}

fn with_query(path: String, parameters: Vec<String>) -> String {
    if parameters.is_empty() {
        path
    } else {
        format!("{path}?{}", parameters.join("&"))
    }
}

fn parse_desk_object_type(value: &str) -> Option<DeskObjectType> {
    match value {
        "annotation" => Some(DeskObjectType::Annotation),
        "learning_item" => Some(DeskObjectType::LearningItem),
        "ai_artifact" => Some(DeskObjectType::AiArtifact),
        _ => None,
    }
}

pub(crate) fn desk_object_type_token(value: DeskObjectType) -> &'static str {
    value.as_str()
}

fn parse_search_source_type(value: &str) -> Option<SearchSourceType> {
    match value {
        "material" => Some(SearchSourceType::Material),
        "highlight" => Some(SearchSourceType::Highlight),
        "note" => Some(SearchSourceType::Note),
        "margin_note" => Some(SearchSourceType::MarginNote),
        "voice_transcript" => Some(SearchSourceType::VoiceTranscript),
        "ai_artifact" => Some(SearchSourceType::AiArtifact),
        "learning_item" => Some(SearchSourceType::LearningItem),
        "shared_comment" => Some(SearchSourceType::SharedComment),
        "shared_chat_message" => Some(SearchSourceType::SharedChatMessage),
        "shared_highlight" => Some(SearchSourceType::SharedHighlight),
        _ => None,
    }
}

pub(crate) fn source_type_token(value: SearchSourceType) -> &'static str {
    value.as_str()
}

pub(crate) fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &value[index + 1..index + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                decoded.push(byte);
                index += 3;
                continue;
            }
        }
        decoded.push(if bytes[index] == b'+' {
            b' '
        } else {
            bytes[index]
        });
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_route_round_trips_anchor() {
        let route = AppRoute::Reader(
            Uuid::nil(),
            Some(ReaderOrigin::LearningSession {
                session_id: Uuid::from_u128(1),
                parent: LearningOrigin::Challenges,
            }),
            Some(Box::new(Anchor {
                revision_id: Uuid::from_u128(2),
                node_path: vec!["глава 1".to_owned(), "node:2".to_owned()],
                end_node_path: vec!["глава 1".to_owned(), "node:2".to_owned()],
                text_range: None,
                quote: String::new(),
                prefix: String::new(),
                suffix: String::new(),
                content_hash: "hash".to_owned(),
                source_locator: None,
                end_source_locator: None,
                page_rects: Vec::new(),
            })),
        );

        assert_eq!(parse_hash(&format!("#{}", route_hash(&route))), Some(route));
    }

    #[test]
    fn community_route_round_trips_social_identity() {
        let route = AppRoute::CommunitySpace(
            Uuid::from_u128(3),
            Some(CommunityTarget::Material {
                shared_material_id: Uuid::from_u128(4),
                source_id: Some(Uuid::from_u128(5)),
            }),
        );

        assert_eq!(parse_hash(&format!("#{}", route_hash(&route))), Some(route));
    }

    #[test]
    fn desk_filters_survive_reload() {
        let route = AppRoute::Desk(DeskRoute {
            view: DeskView::Records(None),
            tag: Some("идея".to_owned()),
            status: Some("active".to_owned()),
            sort: Some("source_order".to_owned()),
        });

        assert_eq!(parse_hash(&format!("#{}", route_hash(&route))), Some(route));
    }
}
