//! Dioxus/DOM adapter for the shared Stage 4 reader contracts.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use gloo_net::http::Request;
use lumi_core::{
    Anchor, AnchorResolution, Annotation, AnnotationId, AnnotationKind, CreateAnnotationCommand,
    DeleteAnnotationCommand, HighlightStyle, LibraryEntry, MoveReadingPositionCommand,
    PageBoundary, PageFragment, PageMap, ReaderNavigation, ReaderPage, ReaderSettings, ReaderTheme,
    ReaderWidth, ReadingDocument, ReadingLink, ReadingLinkKind, ReadingProgress, RenderBlock,
    RenderPlan, TextRange, UpdateAnnotationCommand, UpdateReaderSettingsCommand,
};
use uuid::Uuid;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Element as DomElement, HtmlElement, Node, RequestCredentials};

use super::account::API_BASE;

thread_local! {
    static PAGE_MAP_CACHE: RefCell<HashMap<String, PageMap>> = RefCell::new(HashMap::new());
}

#[derive(Clone)]
struct ReaderView {
    entry: LibraryEntry,
    document: ReadingDocument,
    plan: RenderPlan,
    settings: ReaderSettings,
    page_map: PageMap,
    navigation: ReaderNavigation,
    toc_open: bool,
    settings_open: bool,
    notes_open: bool,
    footnote: Option<ReadingLink>,
    annotations: Vec<AnnotationItem>,
    selected_anchor: Option<Anchor>,
    note_composer_open: bool,
    note_draft: String,
    edit_note_draft: String,
    editing_note: Option<AnnotationId>,
    conflict_draft: Option<String>,
    annotation_message: Option<String>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ItemSyncState {
    Synced,
    Saving,
    Failed,
    Conflicted,
}

#[derive(Clone, PartialEq)]
struct AnnotationItem {
    annotation: Annotation,
    sync_state: ItemSyncState,
    pending: Option<PendingMutation>,
}

#[derive(Clone, PartialEq)]
enum PendingMutation {
    Create {
        command: Box<CreateAnnotationCommand>,
        idempotency_key: String,
    },
    Update {
        command: UpdateAnnotationCommand,
        idempotency_key: String,
    },
    Delete {
        command: DeleteAnnotationCommand,
        idempotency_key: String,
    },
}

#[derive(Clone, Eq, PartialEq)]
struct SaveState {
    pending: usize,
    latest_subject: &'static str,
    failures: BTreeMap<String, String>,
}

impl Default for SaveState {
    fn default() -> Self {
        Self {
            pending: 0,
            latest_subject: "изменения",
            failures: BTreeMap::new(),
        }
    }
}

#[derive(Clone)]
enum ReaderState {
    Loading,
    Ready(Box<ReaderView>),
    Failed(String),
}

#[derive(Clone, Copy)]
enum ReaderPanel {
    Toc,
    Settings,
    Notes,
}

/// API-backed reader route for one ready material.
#[component]
pub(crate) fn ReaderApp(
    material_id: Uuid,
    csrf_token: String,
    on_close: EventHandler<()>,
) -> Element {
    let mut state = use_signal(|| ReaderState::Loading);
    let csrf = use_signal(|| csrf_token);
    let settings_generation = use_signal(|| 0_u64);
    let progress_generation = use_signal(|| 0_u64);
    let settings_in_flight = use_signal(|| false);
    let progress_in_flight = use_signal(|| false);
    let save_state = use_signal(SaveState::default);
    let mut reload_generation = use_signal(|| 0_u64);
    use_effect(move || {
        let _ = reload_generation();
        state.set(ReaderState::Loading);
        spawn(async move {
            match load_reader(material_id).await {
                Ok((entry, document, settings, progress, annotations)) => {
                    let plan = RenderPlan::from_document(&document);
                    match browser_page_map(&plan, settings) {
                        Ok(page_map) => {
                            let mut navigation = ReaderNavigation::default();
                            let restored_page = progress
                                .as_ref()
                                .and_then(|value| {
                                    let offset =
                                        value.locator.text_range.map_or(0, |range| range.start);
                                    page_map.page_for_boundary(&value.locator.node_path, offset)
                                })
                                .unwrap_or_default();
                            navigation.move_to(restored_page, page_map.pages.len());
                            state.set(ReaderState::Ready(Box::new(ReaderView {
                                entry,
                                document,
                                plan,
                                settings,
                                page_map,
                                navigation,
                                toc_open: false,
                                settings_open: false,
                                notes_open: false,
                                footnote: None,
                                annotations: annotations
                                    .into_iter()
                                    .map(|annotation| AnnotationItem {
                                        annotation,
                                        sync_state: ItemSyncState::Synced,
                                        pending: None,
                                    })
                                    .collect(),
                                selected_anchor: None,
                                note_composer_open: false,
                                note_draft: String::new(),
                                edit_note_draft: String::new(),
                                editing_note: None,
                                conflict_draft: None,
                                annotation_message: None,
                            })));
                        }
                        Err(error) => state.set(ReaderState::Failed(error)),
                    }
                }
                Err(error) => state.set(ReaderState::Failed(error)),
            }
        });
    });
    use_effect(move || {
        let footnote_open =
            matches!(&*state.read(), ReaderState::Ready(view) if view.footnote.is_some());
        if footnote_open {
            defer_reader_dialog("reader-footnote-dialog");
        }
    });

    let snapshot = state.read().clone();
    match snapshot {
        ReaderState::Loading => rsx! {
            main { id: "main-content", class: "reader-loading", aria_label: "Загрузка материала", aria_live: "polite",
                span { class: "loading-mark", aria_hidden: "true" }
                h1 { "Готовим страницы…" }
                p { "Lumi загружает нормализованный документ и измеряет раскладку в браузере." }
            }
        },
        ReaderState::Failed(error) => rsx! {
            main { id: "main-content", class: "reader-loading", aria_label: "Ошибка reader",
                p { class: "eyebrow", "Reader unavailable" }
                h1 { "Не удалось открыть материал" }
                p { class: "library-alert", role: "alert", "{error}" }
                div { class: "dialog-actions",
                    button { class: "primary-action", r#type: "button", onclick: move |_| reload_generation += 1, "Повторить" }
                    button { class: "secondary-action", r#type: "button", onclick: move |_| on_close.call(()), "Вернуться в библиотеку" }
                }
            }
        },
        ReaderState::Ready(view) => {
            let page_count = view.page_map.pages.len();
            let current_page = view.navigation.current().min(page_count.saturating_sub(1));
            let page = view.page_map.pages.get(current_page).cloned();
            let title = view.document.title.clone();
            let creators = if view.document.creators.is_empty() {
                "Автор не указан".to_owned()
            } else {
                view.document.creators.join(", ")
            };
            let theme_class = match view.settings.theme {
                ReaderTheme::Paper => "paper",
                ReaderTheme::Night => "night",
            };
            let width_class = match view.settings.width {
                ReaderWidth::Narrow => "narrow",
                ReaderWidth::Balanced => "balanced",
                ReaderWidth::Wide => "wide",
            };
            let export_material_id = view.entry.id;
            let current_save_state = save_state.read().clone();
            let save_label = if current_save_state.pending > 0 {
                format!("Сохраняем {}…", current_save_state.latest_subject)
            } else if let Some(detail) = current_save_state.failures.values().next() {
                format!("Не сохранено: {detail}")
            } else {
                "Сохранено".to_owned()
            };
            let save_class = if current_save_state.pending > 0 {
                "saving"
            } else if current_save_state.failures.is_empty() {
                "saved"
            } else {
                "failed"
            };
            let layout_class = if view.toc_open {
                "toc-open"
            } else if view.notes_open {
                "notes-open"
            } else if view.settings_open {
                "settings-open"
            } else {
                ""
            };
            rsx! {
                main {
                    id: "main-content",
                    class: "reader-workspace {theme_class}",
                    aria_label: "Чтение {title}",
                    style: "--reader-font-size: {view.settings.font_size_px}px; --reader-line-height: {view.settings.line_height_percent}%; --reader-progress: {((current_page + 1) * 100) / page_count.max(1)}%;",
                    onkeydown: move |event| if event.key() == Key::Escape { close_reader_overlay(state); },
                    header { class: "reader-topbar",
                        button { class: "reader-back", r#type: "button", onclick: move |_| on_close.call(()), "← Библиотека" }
                        div { class: "reader-title",
                            h1 { "{title}" }
                            span { "{creators}" }
                        }
                        div { class: "reader-tools", role: "toolbar", aria_label: "Инструменты чтения",
                            button { id: "reader-toc-button", r#type: "button", aria_expanded: view.toc_open, aria_controls: "reader-toc-panel", onclick: move |_| toggle_reader_panel(state, ReaderPanel::Toc), "Оглавление" }
                            button { id: "reader-settings-button", r#type: "button", aria_expanded: view.settings_open, aria_controls: "reader-settings-panel", onclick: move |_| toggle_reader_panel(state, ReaderPanel::Settings), "Настройки" }
                            button { id: "reader-notes-button", r#type: "button", aria_expanded: view.notes_open, aria_controls: "reader-notes-panel", onclick: move |_| {
                                toggle_reader_panel(state, ReaderPanel::Notes);
                            }, "Заметки ({view.annotations.len()})" }
                            button { r#type: "button", onclick: move |_| export_annotations(state, export_material_id), "Экспорт" }
                        }
                        span { class: "reader-save-state {save_class}", role: "status", aria_live: "polite", "{save_label}" }
                        div { class: "reader-chapter-progress", aria_hidden: "true", span {} }
                    }
                    if let Some(message) = view.annotation_message.clone() {
                        p { class: "reader-global-status", role: "alert", aria_live: "assertive", "{message}" }
                    }

                    div { class: "reader-layout {layout_class}",
                        if view.toc_open || view.settings_open || view.notes_open {
                            button { class: "reader-scrim", r#type: "button", aria_label: "Закрыть панель", onclick: move |_| close_reader_overlay(state) }
                        }
                        if view.toc_open {
                            nav { id: "reader-toc-panel", class: "reader-drawer toc-drawer", tabindex: "-1", aria_label: "Оглавление материала",
                                button { class: "focus-sentinel", r#type: "button", aria_label: "Перейти в конец панели", onfocus: move |_| focus_drawer_edge("reader-toc-panel", false) }
                                div { class: "drawer-heading",
                                    h2 { "Оглавление" }
                                    button { id: "reader-toc-close", r#type: "button", aria_label: "Закрыть оглавление", onclick: move |_| {
                                        close_reader_panel(state, ReaderPanel::Toc);
                                    }, "×" }
                                }
                                ol {
                                    for item in view.document.navigation.clone() {
                                        li { button { r#type: "button", onclick: move |_| jump_to_path(state, &item.target_path, csrf, progress_generation, progress_in_flight, save_state), "{item.label}" } }
                                    }
                                }
                                button { class: "focus-sentinel", r#type: "button", aria_label: "Вернуться в начало панели", onfocus: move |_| focus_drawer_edge("reader-toc-panel", true) }
                            }
                        }

                        section { class: "reader-stage {width_class}", aria_label: "Страница книги",
                            div { class: "reader-history", role: "toolbar", aria_label: "История переходов",
                                button { r#type: "button", aria_label: "Назад по истории", disabled: !view.navigation.can_go_back(), onclick: move |_| {
                                    if let ReaderState::Ready(current) = &mut *state.write() { current.navigation.go_back(); persist_current(current, csrf, progress_generation, progress_in_flight, save_state); }
                                }, "↶" }
                                button { r#type: "button", aria_label: "Вперёд по истории", disabled: !view.navigation.can_go_forward(), onclick: move |_| {
                                    if let ReaderState::Ready(current) = &mut *state.write() { current.navigation.go_forward(); persist_current(current, csrf, progress_generation, progress_in_flight, save_state); }
                                }, "↷" }
                            }
                            article { id: "reader-page-surface", class: "reader-page-surface", tabindex: "-1", aria_label: "Страница {current_page + 1} из {page_count}", onmouseup: move |_| capture_browser_selection(state), onkeyup: move |_| capture_browser_selection(state), ontouchend: move |_| capture_browser_selection(state),
                                if let Some(page) = page {
                                    for fragment in page.fragments {
                                        if let Some(block) = view.plan.block(&fragment.node_path).cloned() {
                                            RenderedFragment { block, range: fragment.range, revision_id: view.document.revision_id, plan: view.plan.clone(), annotations: view.annotations.clone(), on_link: move |link: ReadingLink| activate_link(state, link, csrf, progress_generation, progress_in_flight, save_state) }
                                        }
                                    }
                                }
                            }
                            nav { class: "reader-pagination", aria_label: "Навигация по страницам",
                                button { r#type: "button", disabled: current_page == 0, onclick: move |_| move_page(state, current_page.saturating_sub(1), csrf, progress_generation, progress_in_flight, save_state), "← Назад" }
                                div {
                                    span { "{current_page + 1} / {page_count}" }
                                    progress { max: "{page_count}", value: "{current_page + 1}", aria_label: "Прогресс чтения" }
                                }
                                button { r#type: "button", disabled: current_page + 1 >= page_count, onclick: move |_| move_page(state, current_page + 1, csrf, progress_generation, progress_in_flight, save_state), "Дальше →" }
                            }
                        }

                        if view.settings_open {
                            aside { id: "reader-settings-panel", class: "reader-drawer settings-drawer", tabindex: "-1", aria_label: "Настройки чтения",
                                button { class: "focus-sentinel", r#type: "button", aria_label: "Перейти в конец панели", onfocus: move |_| focus_drawer_edge("reader-settings-panel", false) }
                                div { class: "drawer-heading",
                                    h2 { "Настройки" }
                                    button { id: "reader-settings-close", r#type: "button", aria_label: "Закрыть настройки", onclick: move |_| {
                                        close_reader_panel(state, ReaderPanel::Settings);
                                    }, "×" }
                                }
                                fieldset {
                                    legend { "Тема" }
                                    label { input { r#type: "radio", name: "theme", checked: view.settings.theme == ReaderTheme::Paper, onchange: move |_| update_settings(state, csrf, settings_generation, settings_in_flight, save_state, |settings| settings.theme = ReaderTheme::Paper) } "Бумага" }
                                    label { input { r#type: "radio", name: "theme", checked: view.settings.theme == ReaderTheme::Night, onchange: move |_| update_settings(state, csrf, settings_generation, settings_in_flight, save_state, |settings| settings.theme = ReaderTheme::Night) } "Ночь" }
                                }
                                label { class: "settings-control",
                                    span { "Размер текста: {view.settings.font_size_px}px" }
                                    input { r#type: "range", min: "15", max: "30", value: "{view.settings.font_size_px}", aria_label: "Размер текста", oninput: move |event| {
                                        if let Ok(value) = event.value().parse::<u16>() { update_settings(state, csrf, settings_generation, settings_in_flight, save_state, |settings| settings.font_size_px = value); }
                                    } }
                                }
                                fieldset {
                                    legend { "Ширина строки" }
                                    label { input { r#type: "radio", name: "width", checked: view.settings.width == ReaderWidth::Narrow, onchange: move |_| update_settings(state, csrf, settings_generation, settings_in_flight, save_state, |settings| settings.width = ReaderWidth::Narrow) } "Узкая" }
                                    label { input { r#type: "radio", name: "width", checked: view.settings.width == ReaderWidth::Balanced, onchange: move |_| update_settings(state, csrf, settings_generation, settings_in_flight, save_state, |settings| settings.width = ReaderWidth::Balanced) } "Средняя" }
                                    label { input { r#type: "radio", name: "width", checked: view.settings.width == ReaderWidth::Wide, onchange: move |_| update_settings(state, csrf, settings_generation, settings_in_flight, save_state, |settings| settings.width = ReaderWidth::Wide) } "Широкая" }
                                }
                                p { class: "settings-note", "Настройки сохраняются для аккаунта. Карта страниц пересчитывается на этом устройстве." }
                                button { class: "focus-sentinel", r#type: "button", aria_label: "Вернуться в начало панели", onfocus: move |_| focus_drawer_edge("reader-settings-panel", true) }
                            }
                        }

                        if view.notes_open {
                            NotesPanel { state, csrf, save_state, progress_generation, progress_in_flight }
                        }
                    }

                    if let Some(anchor) = view.selected_anchor.clone() {
                        SelectionComposer { state, csrf, save_state, anchor, draft: view.note_draft.clone(), note_composer_open: view.note_composer_open }
                    }

                    if let Some(link) = view.footnote.clone() {
                        dialog { id: "reader-footnote-dialog", class: "footnote-dialog", open: true, tabindex: "-1", aria_modal: "true", aria_label: "Сноска", oncancel: move |event| { event.prevent_default(); close_footnote(state); },
                            p { class: "eyebrow", "Примечание" }
                            if let Some(block) = view.plan.block(&link.target_path) {
                                p { "{block.text.clone().unwrap_or_else(|| link.label.clone())}" }
                            } else {
                                p { "{link.label}" }
                            }
                            button { class: "primary-action", r#type: "button", onclick: move |_| {
                                close_footnote(state);
                            }, "Вернуться к тексту" }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn RenderedFragment(
    block: RenderBlock,
    range: TextRange,
    revision_id: Uuid,
    plan: RenderPlan,
    annotations: Vec<AnnotationItem>,
    on_link: EventHandler<ReadingLink>,
) -> Element {
    let segments = annotation_segments(&block, range, &plan, &annotations);
    let continued = range.start > 0;
    let content = rsx! {
        if continued { span { class: "continued-marker", aria_hidden: "true", "…" } }
        for segment in segments {
            span {
                class: "source-text {segment.class_name}",
                "data-reader-source": "true",
                "data-node-id": "{block.node_id}",
                "data-scalar-start": "{segment.scalar_start}",
                "{segment.text}"
            }
        }
        for link in block.links.iter().filter(|link| ranges_intersect(link.text_range, range)).cloned() {
            if link.kind == ReadingLinkKind::External {
                if let Some(url) = safe_external_url(&link) {
                    a { class: "inline-link", href: "{url}", target: "_blank", rel: "noopener noreferrer", "Открыть: {link.label}" }
                }
            } else {
                button { class: "inline-link", r#type: "button", onclick: move |_| on_link.call(link.clone()), "Перейти: {link.label}" }
            }
        }
    };
    match block.kind {
        lumi_core::ReadingNodeKind::Heading { level: 1 } => {
            rsx! { h2 { id: "{block.node_id}", class: "reading-heading", {content} } }
        }
        lumi_core::ReadingNodeKind::Heading { level: 2 } => {
            rsx! { h3 { id: "{block.node_id}", class: "reading-heading", {content} } }
        }
        lumi_core::ReadingNodeKind::Heading { level: 3 } => {
            rsx! { h4 { id: "{block.node_id}", class: "reading-heading", {content} } }
        }
        lumi_core::ReadingNodeKind::Heading { level: 4 } => {
            rsx! { h5 { id: "{block.node_id}", class: "reading-heading", {content} } }
        }
        lumi_core::ReadingNodeKind::Heading { .. } => {
            rsx! { h6 { id: "{block.node_id}", class: "reading-heading", {content} } }
        }
        lumi_core::ReadingNodeKind::Blockquote => {
            rsx! { blockquote { id: "{block.node_id}", {content} } }
        }
        lumi_core::ReadingNodeKind::ListItem => {
            rsx! { ul { id: "{block.node_id}", li { {content} } } }
        }
        lumi_core::ReadingNodeKind::CodeBlock => {
            rsx! { pre { id: "{block.node_id}", code { {content} } } }
        }
        lumi_core::ReadingNodeKind::Table => {
            rsx! { div { id: "{block.node_id}", class: "reading-table", role: "table", {content} } }
        }
        lumi_core::ReadingNodeKind::HorizontalRule => rsx! { hr { id: "{block.node_id}" } },
        lumi_core::ReadingNodeKind::Image => {
            let alt = block.text.unwrap_or_else(|| "Иллюстрация".to_owned());
            let src = block
                .resource_hash
                .map(|hash| format!("{API_BASE}/revisions/{revision_id}/resources/{hash}"));
            rsx! { figure { id: "{block.node_id}", if let Some(src) = src { img { src, alt: "{alt}", loading: "lazy" } } else { div { class: "image-placeholder", "{alt}" } } } }
        }
        lumi_core::ReadingNodeKind::Caption => {
            rsx! { p { id: "{block.node_id}", class: "reading-caption", {content} } }
        }
        lumi_core::ReadingNodeKind::Footnote => {
            rsx! { aside { id: "{block.node_id}", class: "reading-footnote", {content} } }
        }
        lumi_core::ReadingNodeKind::PluginPlaceholder { .. } => {
            rsx! { div { id: "{block.node_id}", class: "plugin-placeholder", "Неподдерживаемый интерактивный блок" } }
        }
        _ => rsx! { p { id: "{block.node_id}", class: "reading-paragraph", {content} } },
    }
}

#[derive(Clone)]
struct TextSegment {
    text: String,
    scalar_start: usize,
    class_name: &'static str,
}

fn annotation_segments(
    block: &RenderBlock,
    visible: TextRange,
    plan: &RenderPlan,
    annotations: &[AnnotationItem],
) -> Vec<TextSegment> {
    let text = block.text.as_deref().unwrap_or_default();
    let chars: Vec<char> = scalar_slice(text, visible.start, visible.end)
        .chars()
        .collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let mut classes = vec![""; chars.len()];
    for item in annotations {
        let resolved = match plan.resolve_anchor(&item.annotation.anchor) {
            AnchorResolution::Resolved { anchor, .. } => anchor,
            AnchorResolution::Unresolved => continue,
        };
        let Some(annotation_range) = plan.anchor_range_for_block(&resolved, &block.node_path)
        else {
            continue;
        };
        let start = annotation_range.start.max(visible.start);
        let end = annotation_range.end.min(visible.end);
        let class_name = match item.annotation.kind {
            AnnotationKind::Highlight { .. } => "annotation-highlight",
            AnnotationKind::Note { .. } => "annotation-note",
        };
        for scalar in start..end {
            if let Some(value) = classes.get_mut(scalar.saturating_sub(visible.start)) {
                *value = class_name;
            }
        }
    }
    let mut output = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let class_name = classes[start];
        let mut end = start + 1;
        while end < chars.len() && classes[end] == class_name {
            end += 1;
        }
        output.push(TextSegment {
            text: chars[start..end].iter().collect(),
            scalar_start: visible.start + start,
            class_name,
        });
        start = end;
    }
    output
}

#[component]
fn NotesPanel(
    state: Signal<ReaderState>,
    csrf: Signal<String>,
    save_state: Signal<SaveState>,
    progress_generation: Signal<u64>,
    progress_in_flight: Signal<bool>,
) -> Element {
    let snapshot = state.read().clone();
    let ReaderState::Ready(view) = snapshot else {
        return rsx! {};
    };
    rsx! {
        aside { id: "reader-notes-panel", class: "reader-drawer notes-drawer", tabindex: "-1", aria_label: "Личные заметки и выделения", onkeydown: move |event| {
            if event.key() == Key::Escape { close_reader_panel(state, ReaderPanel::Notes); }
        },
            button { class: "focus-sentinel", r#type: "button", aria_label: "Перейти в конец панели", onfocus: move |_| focus_drawer_edge("reader-notes-panel", false) }
            div { class: "drawer-heading",
                div { h2 { "Заметки" } p { class: "private-label", "Только для вас" } }
                button { id: "reader-notes-close", r#type: "button", aria_label: "Закрыть заметки", onclick: move |_| close_reader_panel(state, ReaderPanel::Notes), "×" }
            }
            if view.annotations.is_empty() {
                p { class: "notes-empty", "Выделите фрагмент на странице, чтобы сохранить highlight или заметку." }
            } else {
                ol { class: "annotation-list",
                    for item in view.annotations.clone() {
                        AnnotationPanelItem { state, csrf, save_state, progress_generation, progress_in_flight, item, editing: view.editing_note, draft: view.edit_note_draft.clone() }
                    }
                }
            }
            if let Some(draft) = view.conflict_draft { details { open: true, summary { "Несохранённая версия" } p { "{draft}" } } }
            button { class: "focus-sentinel", r#type: "button", aria_label: "Вернуться в начало панели", onfocus: move |_| focus_drawer_edge("reader-notes-panel", true) }
        }
    }
}

#[component]
fn SelectionComposer(
    state: Signal<ReaderState>,
    csrf: Signal<String>,
    save_state: Signal<SaveState>,
    anchor: Anchor,
    draft: String,
    note_composer_open: bool,
) -> Element {
    let highlight_anchor = anchor.clone();
    rsx! {
        if !note_composer_open {
            div { class: "selection-actions", role: "toolbar", aria_label: "Действия с выделением",
                button { r#type: "button", onclick: move |_| create_highlight(state, highlight_anchor.clone(), csrf, save_state), "Выделить" }
                button { r#type: "button", onclick: move |_| {
                    if let ReaderState::Ready(current) = &mut *state.write() {
                        current.note_draft.clear();
                        current.note_composer_open = true;
                        current.annotation_message = None;
                    }
                    defer_reader_focus("reader-note-draft");
                }, "Заметка" }
                button { r#type: "button", onclick: move |_| dismiss_selection(state), "Отмена" }
            }
        } else {
            form { class: "note-composer", onsubmit: move |event| { event.prevent_default(); create_note(state, anchor.clone(), csrf, save_state); },
                label { "Текст заметки", textarea { id: "reader-note-draft", name: "note_body", autocomplete: "off", placeholder: "Добавьте мысль…", value: "{draft}", oninput: move |event| if let ReaderState::Ready(current) = &mut *state.write() { current.note_draft = event.value(); } } }
                div { class: "dialog-actions",
                    button { class: "secondary-action", r#type: "button", onclick: move |_| dismiss_selection(state), "Отмена" }
                    button { class: "primary-action", r#type: "submit", disabled: draft.trim().is_empty(), "Сохранить заметку" }
                }
            }
        }
    }
}

#[component]
fn AnnotationPanelItem(
    state: Signal<ReaderState>,
    csrf: Signal<String>,
    save_state: Signal<SaveState>,
    progress_generation: Signal<u64>,
    progress_in_flight: Signal<bool>,
    item: AnnotationItem,
    editing: Option<AnnotationId>,
    draft: String,
) -> Element {
    let target_anchor = item.annotation.anchor.clone();
    let delete_value = item.annotation.clone();
    let retry_value = item.annotation.clone();
    let edit_value = item.annotation.clone();
    let annotation_id = item.annotation.id;
    let quote = item.annotation.anchor.quote.clone();
    rsx! {
        li { class: "annotation-item",
            button { class: "annotation-target", r#type: "button", onclick: move |_| navigate_to_annotation(state, &target_anchor, csrf, progress_generation, progress_in_flight, save_state), blockquote { "{quote}" } }
            match item.annotation.kind.clone() {
                AnnotationKind::Highlight { style } => rsx! { p { class: "annotation-kind", "Highlight · {style:?}" } },
                AnnotationKind::Note { body } => rsx! {
                    p { class: "annotation-kind", "Заметка" }
                    p { "{body}" }
                    button { r#type: "button", onclick: move |_| if let ReaderState::Ready(current) = &mut *state.write() { current.editing_note = Some(annotation_id); current.edit_note_draft = body.clone(); current.conflict_draft = None; }, "Изменить" }
                },
            }
            button { class: "danger-link", r#type: "button", onclick: move |_| delete_annotation_optimistic(state, delete_value.clone(), csrf, save_state), "Удалить" }
            if item.sync_state == ItemSyncState::Saving { span { role: "status", "Сохраняем…" } }
            if item.sync_state == ItemSyncState::Failed { button { r#type: "button", onclick: move |_| retry_failed_annotation(state, retry_value.clone(), csrf, save_state), "Повторить" } }
            if item.sync_state == ItemSyncState::Conflicted { p { class: "annotation-conflict", role: "alert", "Заметка изменилась в другом окне. Ваш текст сохранён в редакторе." } }
            if editing == Some(annotation_id) {
                form { class: "note-editor", onsubmit: move |event| { event.prevent_default(); update_note_optimistic(state, edit_value.clone(), csrf, save_state); },
                    label { "Редактировать заметку", textarea { name: "edited_note_body", autocomplete: "off", value: "{draft}", oninput: move |event| if let ReaderState::Ready(current) = &mut *state.write() { current.edit_note_draft = event.value(); } } }
                    button { r#type: "submit", disabled: draft.trim().is_empty(), "Сохранить изменения" }
                }
            }
        }
    }
}

fn capture_browser_selection(mut state: Signal<ReaderState>) {
    let result = (|| -> Result<Anchor, String> {
        let window = web_sys::window().ok_or_else(|| "Browser window недоступен".to_owned())?;
        let selection = window
            .get_selection()
            .map_err(|_| "Browser Selection недоступен".to_owned())?
            .ok_or_else(|| "Выделение пусто".to_owned())?;
        if selection.is_collapsed() || selection.range_count() == 0 {
            return Err("Выделение пусто".to_owned());
        }
        let range = selection
            .get_range_at(0)
            .map_err(|_| "Не удалось прочитать browser Range".to_owned())?;
        let start_node = range
            .start_container()
            .map_err(|_| "Начало выделения вне текста книги".to_owned())?;
        let end_node = range
            .end_container()
            .map_err(|_| "Конец выделения вне текста книги".to_owned())?;
        let ReaderState::Ready(view) = &*state.read() else {
            return Err("Reader ещё не готов".to_owned());
        };
        let (start_path, start_offset) = selection_boundary(
            &view.plan,
            &start_node,
            range.start_offset().unwrap_or_default(),
        )?;
        let (end_path, end_offset) = selection_boundary(
            &view.plan,
            &end_node,
            range.end_offset().unwrap_or_default(),
        )?;
        let anchor = view
            .plan
            .anchor_from_selection(&start_path, start_offset, &end_path, end_offset)
            .map_err(|error| error.to_string())?;
        selection
            .remove_all_ranges()
            .map_err(|_| "Не удалось очистить browser Selection".to_owned())?;
        Ok(anchor)
    })();
    if let ReaderState::Ready(view) = &mut *state.write() {
        match result {
            Ok(anchor) => {
                view.selected_anchor = Some(anchor);
                view.annotation_message = None;
            }
            Err(error) if error != "Выделение пусто" => {
                view.annotation_message = Some(error)
            }
            Err(_) => {}
        }
    }
}

fn selection_boundary(
    plan: &RenderPlan,
    node: &Node,
    utf16_offset: u32,
) -> Result<(Vec<String>, usize), String> {
    let element = source_element(node)
        .ok_or_else(|| "Выделение должно начинаться и заканчиваться в тексте книги".to_owned())?;
    let node_id = element
        .get_attribute("data-node-id")
        .ok_or_else(|| "DOM fragment не содержит stable node id".to_owned())?;
    let block = plan
        .blocks
        .iter()
        .find(|block| block.node_id == node_id)
        .ok_or_else(|| "DOM fragment не принадлежит ReadingDocument".to_owned())?;
    let fragment_start = element
        .get_attribute("data-scalar-start")
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| "DOM fragment не содержит scalar offset".to_owned())?;
    let fragment_text = element.text_content().unwrap_or_default();
    let boundary_utf16 = if node.dyn_ref::<DomElement>().is_some() {
        let children = node.child_nodes();
        let child_limit = utf16_offset.min(children.length());
        let mut length = 0_u32;
        for index in 0..child_limit {
            if let Some(child) = children.item(index) {
                length = length.saturating_add(
                    child
                        .text_content()
                        .unwrap_or_default()
                        .encode_utf16()
                        .count()
                        .min(u32::MAX as usize) as u32,
                );
            }
        }
        length
    } else {
        utf16_offset
    };
    let local_offset = scalar_offset_from_utf16(&fragment_text, boundary_utf16)?;
    Ok((block.node_path.clone(), fragment_start + local_offset))
}

fn source_element(node: &Node) -> Option<DomElement> {
    let mut current = Some(node.clone());
    while let Some(node) = current {
        if let Some(element) = node.dyn_ref::<DomElement>() {
            if element.get_attribute("data-reader-source").as_deref() == Some("true") {
                return Some(element.clone());
            }
        }
        current = node.parent_node();
    }
    None
}

fn scalar_offset_from_utf16(text: &str, offset: u32) -> Result<usize, String> {
    let mut utf16 = 0_u32;
    for (scalar, character) in text.chars().enumerate() {
        if utf16 == offset {
            return Ok(scalar);
        }
        utf16 = utf16.saturating_add(character.len_utf16() as u32);
        if utf16 > offset {
            return Err("Граница Selection попала внутрь Unicode scalar".to_owned());
        }
    }
    if utf16 == offset {
        Ok(text.chars().count())
    } else {
        Err("Граница Selection выходит за пределы fragment".to_owned())
    }
}

fn create_highlight(
    state: Signal<ReaderState>,
    anchor: Anchor,
    csrf: Signal<String>,
    save_state: Signal<SaveState>,
) {
    create_annotation_optimistic(
        state,
        anchor,
        AnnotationKind::Highlight {
            style: HighlightStyle::Yellow,
        },
        csrf,
        save_state,
    );
}

fn create_note(
    state: Signal<ReaderState>,
    anchor: Anchor,
    csrf: Signal<String>,
    save_state: Signal<SaveState>,
) {
    let body = match &*state.read() {
        ReaderState::Ready(view) => view.note_draft.trim().to_owned(),
        _ => return,
    };
    if !body.is_empty() {
        create_annotation_optimistic(
            state,
            anchor,
            AnnotationKind::Note { body },
            csrf,
            save_state,
        );
    }
}

fn create_annotation_optimistic(
    mut state: Signal<ReaderState>,
    anchor: Anchor,
    kind: AnnotationKind,
    csrf: Signal<String>,
    save_state: Signal<SaveState>,
) {
    let (material_id, revision_id) = match &*state.read() {
        ReaderState::Ready(view) => (view.entry.id, view.document.revision_id),
        _ => return,
    };
    let temporary_id = Uuid::now_v7();
    let temporary = Annotation {
        id: temporary_id,
        material_id,
        revision_id,
        anchor: anchor.clone(),
        kind: kind.clone(),
        revision: 0,
        created_at: 0,
        updated_at: 0,
    };
    let pending = PendingMutation::Create {
        command: Box::new(CreateAnnotationCommand {
            material_id,
            revision_id,
            anchor,
            kind,
        }),
        idempotency_key: Uuid::now_v7().to_string(),
    };
    if let ReaderState::Ready(view) = &mut *state.write() {
        view.annotations.push(AnnotationItem {
            annotation: temporary,
            sync_state: ItemSyncState::Saving,
            pending: Some(pending.clone()),
        });
        view.selected_anchor = None;
        view.note_composer_open = false;
        view.note_draft.clear();
        view.annotation_message = None;
    }
    begin_save(save_state, pending_key(&pending), pending_subject(&pending));
    dispatch_pending(state, temporary_id, pending, csrf, save_state);
}

fn retry_failed_annotation(
    mut state: Signal<ReaderState>,
    annotation: Annotation,
    csrf: Signal<String>,
    save_state: Signal<SaveState>,
) {
    let pending = match &*state.read() {
        ReaderState::Ready(view) => view
            .annotations
            .iter()
            .find(|item| item.annotation.id == annotation.id)
            .and_then(|item| item.pending.clone()),
        _ => None,
    };
    if let Some(pending) = pending {
        if let ReaderState::Ready(view) = &mut *state.write() {
            if let Some(item) = view
                .annotations
                .iter_mut()
                .find(|item| item.annotation.id == annotation.id)
            {
                item.sync_state = ItemSyncState::Saving;
            }
        }
        begin_save(save_state, pending_key(&pending), pending_subject(&pending));
        dispatch_pending(state, annotation.id, pending, csrf, save_state);
    }
}

fn update_note_optimistic(
    mut state: Signal<ReaderState>,
    previous: Annotation,
    csrf: Signal<String>,
    save_state: Signal<SaveState>,
) {
    let draft = match &*state.read() {
        ReaderState::Ready(view) => view.edit_note_draft.trim().to_owned(),
        _ => return,
    };
    if draft.is_empty() {
        return;
    }
    if let ReaderState::Ready(view) = &mut *state.write() {
        if let Some(item) = view
            .annotations
            .iter_mut()
            .find(|item| item.annotation.id == previous.id)
        {
            item.annotation.kind = AnnotationKind::Note {
                body: draft.clone(),
            };
            item.sync_state = ItemSyncState::Saving;
        }
        view.editing_note = None;
    }
    let command = UpdateAnnotationCommand {
        material_id: previous.material_id,
        annotation_id: previous.id,
        expected_revision: previous.revision,
        kind: AnnotationKind::Note {
            body: draft.clone(),
        },
    };
    let pending = PendingMutation::Update {
        command,
        idempotency_key: Uuid::now_v7().to_string(),
    };
    begin_save(save_state, pending_key(&pending), pending_subject(&pending));
    if let ReaderState::Ready(view) = &mut *state.write() {
        if let Some(item) = view
            .annotations
            .iter_mut()
            .find(|item| item.annotation.id == previous.id)
        {
            item.pending = Some(pending.clone());
        }
    }
    dispatch_pending(state, previous.id, pending, csrf, save_state);
}

fn delete_annotation_optimistic(
    mut state: Signal<ReaderState>,
    annotation: Annotation,
    csrf: Signal<String>,
    save_state: Signal<SaveState>,
) {
    let confirmed = web_sys::window()
        .and_then(|window| window.confirm_with_message("Удалить эту аннотацию?").ok())
        .unwrap_or(false);
    if !confirmed {
        return;
    }
    let command = DeleteAnnotationCommand {
        material_id: annotation.material_id,
        annotation_id: annotation.id,
        expected_revision: annotation.revision,
    };
    let pending = PendingMutation::Delete {
        command,
        idempotency_key: Uuid::now_v7().to_string(),
    };
    if let ReaderState::Ready(view) = &mut *state.write() {
        if let Some(item) = view
            .annotations
            .iter_mut()
            .find(|item| item.annotation.id == annotation.id)
        {
            item.sync_state = ItemSyncState::Saving;
            item.pending = Some(pending.clone());
        }
    }
    begin_save(save_state, pending_key(&pending), pending_subject(&pending));
    dispatch_pending(state, annotation.id, pending, csrf, save_state);
}

fn replace_annotation(
    state: &mut Signal<ReaderState>,
    old_id: AnnotationId,
    annotation: Annotation,
) {
    if let ReaderState::Ready(view) = &mut *state.write() {
        if let Some(item) = view
            .annotations
            .iter_mut()
            .find(|item| item.annotation.id == old_id)
        {
            item.annotation = annotation;
            item.sync_state = ItemSyncState::Synced;
            item.pending = None;
        }
        view.annotation_message = None;
        view.conflict_draft = None;
    }
}

fn dispatch_pending(
    mut state: Signal<ReaderState>,
    local_id: AnnotationId,
    pending: PendingMutation,
    csrf: Signal<String>,
    save_state: Signal<SaveState>,
) {
    let csrf_token = csrf.read().clone();
    let save_key = pending_key(&pending).to_owned();
    spawn_forever(async move {
        let result = match &pending {
            PendingMutation::Create {
                command,
                idempotency_key,
            } => post_annotation(command, idempotency_key, &csrf_token).await,
            PendingMutation::Update {
                command,
                idempotency_key,
            } => put_annotation(command, idempotency_key, &csrf_token).await,
            PendingMutation::Delete {
                command,
                idempotency_key,
            } => delete_annotation_api(command, idempotency_key, &csrf_token).await,
        };
        match result {
            Ok(annotation) => {
                if matches!(pending, PendingMutation::Delete { .. }) {
                    if let ReaderState::Ready(view) = &mut *state.write() {
                        view.annotations
                            .retain(|item| item.annotation.id != local_id);
                    }
                } else {
                    replace_annotation(&mut state, local_id, annotation);
                }
                finish_save(save_state, &save_key, Ok(()));
            }
            Err(ApiMutationError::Conflict) => {
                let (material_id, draft) = match &*state.read() {
                    ReaderState::Ready(view) => (view.entry.id, view.edit_note_draft.clone()),
                    _ => return,
                };
                match get_json::<Vec<Annotation>>(&format!("/materials/{material_id}/annotations"))
                    .await
                {
                    Ok(annotations) => {
                        if let ReaderState::Ready(view) = &mut *state.write() {
                            let server_annotation = annotations
                                .into_iter()
                                .find(|annotation| annotation.id == local_id);
                            match (
                                view.annotations
                                    .iter_mut()
                                    .find(|item| item.annotation.id == local_id),
                                server_annotation,
                            ) {
                                (Some(item), Some(annotation)) => {
                                    item.annotation = annotation;
                                    item.sync_state = ItemSyncState::Conflicted;
                                    item.pending = None;
                                }
                                (None, Some(annotation)) => view.annotations.push(AnnotationItem {
                                    annotation,
                                    sync_state: ItemSyncState::Conflicted,
                                    pending: None,
                                }),
                                (Some(item), None) => {
                                    item.sync_state = ItemSyncState::Conflicted;
                                    item.pending = None;
                                }
                                (None, None) => {}
                            }
                            view.conflict_draft = (!draft.is_empty()).then_some(draft);
                            view.editing_note = view
                                .annotations
                                .iter()
                                .any(|item| item.annotation.id == local_id)
                                .then_some(local_id);
                            view.annotation_message = Some(
                                "Серверная версия загружена; ваш текст сохранён отдельно"
                                    .to_owned(),
                            );
                        }
                        finish_save(save_state, &save_key, Ok(()));
                    }
                    Err(error) => {
                        mark_annotation_failed(&mut state, local_id, error.clone());
                        finish_save(save_state, &save_key, Err(error));
                    }
                }
            }
            Err(error) => {
                mark_annotation_failed(&mut state, local_id, error.message());
                finish_save(save_state, &save_key, Err(error.message()));
            }
        }
    });
}

fn pending_subject(pending: &PendingMutation) -> &'static str {
    match pending {
        PendingMutation::Create { .. } => "аннотацию",
        PendingMutation::Update { .. } => "заметку",
        PendingMutation::Delete { .. } => "удаление",
    }
}

fn pending_key(pending: &PendingMutation) -> &str {
    match pending {
        PendingMutation::Create {
            idempotency_key, ..
        }
        | PendingMutation::Update {
            idempotency_key, ..
        }
        | PendingMutation::Delete {
            idempotency_key, ..
        } => idempotency_key,
    }
}

fn begin_save(mut save_state: Signal<SaveState>, key: &str, subject: &'static str) {
    let mut current = save_state.write();
    current.pending = current.pending.saturating_add(1);
    current.latest_subject = subject;
    current.failures.remove(key);
}

fn finish_save(mut save_state: Signal<SaveState>, key: &str, result: Result<(), String>) {
    let mut current = save_state.write();
    current.pending = current.pending.saturating_sub(1);
    if let Err(error) = result {
        current.failures.insert(key.to_owned(), error);
    }
}

fn mark_annotation_failed(state: &mut Signal<ReaderState>, id: AnnotationId, message: String) {
    if let ReaderState::Ready(view) = &mut *state.write() {
        if let Some(item) = view
            .annotations
            .iter_mut()
            .find(|item| item.annotation.id == id)
        {
            item.sync_state = ItemSyncState::Failed;
        }
        view.annotation_message = Some(message);
    }
}

fn navigate_to_annotation(
    mut state: Signal<ReaderState>,
    anchor: &Anchor,
    csrf: Signal<String>,
    generation: Signal<u64>,
    in_flight: Signal<bool>,
    save_state: Signal<SaveState>,
) {
    if let ReaderState::Ready(view) = &mut *state.write() {
        let resolved = match view.plan.resolve_anchor(anchor) {
            AnchorResolution::Resolved { anchor, .. } => anchor,
            AnchorResolution::Unresolved => {
                view.annotation_message =
                    Some("Anchor не удалось разрешить в текущей версии".to_owned());
                return;
            }
        };
        let offset = resolved.text_range.map_or(0, |range| range.start);
        if let Some(page) = view.page_map.page_for_boundary(&resolved.node_path, offset) {
            view.navigation.jump_to(page, view.page_map.pages.len());
            view.notes_open = false;
            let node_id = view
                .plan
                .block(&resolved.node_path)
                .map(|block| block.node_id.clone());
            persist_current(view, csrf, generation, in_flight, save_state);
            if let Some(node_id) = node_id {
                spawn(async move {
                    browser_delay(20).await;
                    focus_reader_node(&node_id);
                });
            }
        } else {
            view.annotation_message =
                Some("Anchor не удалось разрешить в текущей версии".to_owned());
        }
    }
}

fn focus_reader_node(node_id: &str) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Some(element) = document.get_element_by_id(node_id) else {
        return;
    };
    let _ = element.set_attribute("tabindex", "-1");
    if let Ok(element) = element.dyn_into::<HtmlElement>() {
        let _ = element.focus();
    }
}

fn focus_drawer_edge(panel_id: &str, first: bool) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Some(panel) = document.get_element_by_id(panel_id) else {
        return;
    };
    let Ok(nodes) = panel.query_selector_all(
        "button:not(.focus-sentinel):not([disabled]), a[href], input:not([disabled]), textarea:not([disabled])",
    ) else {
        return;
    };
    let index = if first {
        0
    } else {
        nodes.length().saturating_sub(1)
    };
    let Some(target) = nodes
        .item(index)
        .and_then(|node| node.dyn_into::<HtmlElement>().ok())
    else {
        return;
    };
    let _ = target.focus();
}

fn toggle_reader_panel(mut state: Signal<ReaderState>, panel: ReaderPanel) {
    let (open, target) = if let ReaderState::Ready(view) = &mut *state.write() {
        let was_open = match panel {
            ReaderPanel::Toc => view.toc_open,
            ReaderPanel::Settings => view.settings_open,
            ReaderPanel::Notes => view.notes_open,
        };
        view.toc_open = false;
        view.settings_open = false;
        view.notes_open = false;
        if !was_open {
            match panel {
                ReaderPanel::Toc => view.toc_open = true,
                ReaderPanel::Settings => view.settings_open = true,
                ReaderPanel::Notes => view.notes_open = true,
            }
        }
        let target = match panel {
            ReaderPanel::Toc => "reader-toc-close",
            ReaderPanel::Settings => "reader-settings-close",
            ReaderPanel::Notes => "reader-notes-close",
        };
        (!was_open, target)
    } else {
        return;
    };
    if open {
        defer_reader_focus(target);
    } else {
        defer_reader_focus(panel_trigger(panel));
    }
}

fn close_reader_panel(mut state: Signal<ReaderState>, panel: ReaderPanel) {
    if let ReaderState::Ready(view) = &mut *state.write() {
        match panel {
            ReaderPanel::Toc => view.toc_open = false,
            ReaderPanel::Settings => view.settings_open = false,
            ReaderPanel::Notes => view.notes_open = false,
        }
    }
    defer_reader_focus(panel_trigger(panel));
}

fn panel_trigger(panel: ReaderPanel) -> &'static str {
    match panel {
        ReaderPanel::Toc => "reader-toc-button",
        ReaderPanel::Settings => "reader-settings-button",
        ReaderPanel::Notes => "reader-notes-button",
    }
}

fn close_reader_overlay(mut state: Signal<ReaderState>) {
    let target = if let ReaderState::Ready(view) = &mut *state.write() {
        if view.footnote.take().is_some() {
            Some("reader-page-surface")
        } else if view.toc_open {
            view.toc_open = false;
            Some("reader-toc-button")
        } else if view.settings_open {
            view.settings_open = false;
            Some("reader-settings-button")
        } else if view.notes_open {
            view.notes_open = false;
            Some("reader-notes-button")
        } else if view.selected_anchor.is_some() {
            view.selected_anchor = None;
            view.note_composer_open = false;
            Some("reader-page-surface")
        } else {
            None
        }
    } else {
        None
    };
    if let Some(target) = target {
        defer_reader_focus(target);
    }
}

fn dismiss_selection(mut state: Signal<ReaderState>) {
    if let ReaderState::Ready(view) = &mut *state.write() {
        view.selected_anchor = None;
        view.note_composer_open = false;
        view.note_draft.clear();
    }
    defer_reader_focus("reader-page-surface");
}

fn close_footnote(mut state: Signal<ReaderState>) {
    if let ReaderState::Ready(view) = &mut *state.write() {
        view.footnote = None;
    }
    defer_reader_focus("reader-page-surface");
}

fn defer_reader_focus(id: &str) {
    let id = id.to_owned();
    spawn_forever(async move {
        browser_delay(20).await;
        focus_reader_node(&id);
    });
}

fn defer_reader_dialog(id: &str) {
    let id = id.to_owned();
    spawn_forever(async move {
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

fn export_annotations(mut state: Signal<ReaderState>, material_id: Uuid) {
    spawn(async move {
        let response = match Request::get(&format!(
            "{API_BASE}/materials/{material_id}/annotations/export"
        ))
        .credentials(RequestCredentials::Include)
        .send()
        .await
        {
            Ok(response) => response,
            Err(error) => {
                set_annotation_message(&mut state, format!("Экспорт не выполнен: {error}"));
                return;
            }
        };
        if !response.ok() {
            if response.status() == 401 {
                super::account::notify_session_expired();
            }
            set_annotation_message(
                &mut state,
                format!(
                    "Экспорт не выполнен: Lumi API вернул HTTP {}.",
                    response.status()
                ),
            );
            return;
        }
        let json = match response.text().await {
            Ok(json) => json,
            Err(error) => {
                set_annotation_message(&mut state, format!("Экспорт не выполнен: {error}"));
                return;
            }
        };
        let parts = js_sys::Array::new();
        parts.push(&wasm_bindgen::JsValue::from_str(&json));
        let options = web_sys::BlobPropertyBag::new();
        options.set_type("application/json");
        let Ok(blob) = web_sys::Blob::new_with_str_sequence_and_options(&parts, &options) else {
            return;
        };
        let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
            return;
        };
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        let Ok(element) = document.create_element("a") else {
            return;
        };
        let _ = element.set_attribute("href", &url);
        let _ = element.set_attribute("download", &format!("lumi-annotations-{material_id}.json"));
        if let Ok(element) = element.dyn_into::<HtmlElement>() {
            element.click();
        }
        let _ = web_sys::Url::revoke_object_url(&url);
        set_annotation_message(&mut state, "Экспорт подготовлен".to_owned());
    });
}

fn set_annotation_message(state: &mut Signal<ReaderState>, message: String) {
    if let ReaderState::Ready(view) = &mut *state.write() {
        view.annotation_message = Some(message);
    }
}

#[derive(Clone, Eq, PartialEq)]
enum ApiMutationError {
    Conflict,
    Unauthorized,
    Other(String),
}

impl ApiMutationError {
    fn message(&self) -> String {
        match self {
            Self::Conflict => "Конфликт версии: сервер сохранил более новую запись".to_owned(),
            Self::Unauthorized => "Сессия истекла — войдите снова".to_owned(),
            Self::Other(message) => message.clone(),
        }
    }
}

async fn post_annotation(
    command: &CreateAnnotationCommand,
    idempotency_key: &str,
    csrf: &str,
) -> Result<Annotation, ApiMutationError> {
    annotation_request(
        Request::post(&format!(
            "{API_BASE}/materials/{}/annotations",
            command.material_id
        )),
        command,
        idempotency_key,
        csrf,
    )
    .await
}

async fn put_annotation(
    command: &UpdateAnnotationCommand,
    idempotency_key: &str,
    csrf: &str,
) -> Result<Annotation, ApiMutationError> {
    annotation_request(
        Request::put(&format!(
            "{API_BASE}/materials/{}/annotations/{}",
            command.material_id, command.annotation_id
        )),
        command,
        idempotency_key,
        csrf,
    )
    .await
}

async fn delete_annotation_api(
    command: &DeleteAnnotationCommand,
    idempotency_key: &str,
    csrf: &str,
) -> Result<Annotation, ApiMutationError> {
    annotation_request(
        Request::delete(&format!(
            "{API_BASE}/materials/{}/annotations/{}",
            command.material_id, command.annotation_id
        )),
        command,
        idempotency_key,
        csrf,
    )
    .await
}

async fn annotation_request<T: serde::Serialize>(
    request: gloo_net::http::RequestBuilder,
    command: &T,
    idempotency_key: &str,
    csrf: &str,
) -> Result<Annotation, ApiMutationError> {
    let request = request
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .header("Idempotency-Key", idempotency_key)
        .json(command)
        .map_err(|error| ApiMutationError::Other(error.to_string()))?;
    let response = request
        .send()
        .await
        .map_err(|error| ApiMutationError::Other(error.to_string()))?;
    match response.status() {
        200..=299 => response
            .json()
            .await
            .map_err(|error| ApiMutationError::Other(error.to_string())),
        401 => {
            super::account::notify_session_expired();
            Err(ApiMutationError::Unauthorized)
        }
        409 => Err(ApiMutationError::Conflict),
        status => Err(ApiMutationError::Other(format!(
            "Lumi API вернул HTTP {status}"
        ))),
    }
}

fn move_page(
    mut state: Signal<ReaderState>,
    page: usize,
    csrf: Signal<String>,
    generation: Signal<u64>,
    in_flight: Signal<bool>,
    save_state: Signal<SaveState>,
) {
    if let ReaderState::Ready(current) = &mut *state.write() {
        current
            .navigation
            .move_to(page, current.page_map.pages.len());
        persist_current(current, csrf, generation, in_flight, save_state);
    }
}

fn safe_external_url(link: &ReadingLink) -> Option<String> {
    let value = link.external_url.as_deref()?;
    let lowered = value.to_ascii_lowercase();
    (lowered.starts_with("https://") || lowered.starts_with("http://")).then(|| value.to_owned())
}

fn jump_to_path(
    mut state: Signal<ReaderState>,
    path: &[String],
    csrf: Signal<String>,
    generation: Signal<u64>,
    in_flight: Signal<bool>,
    save_state: Signal<SaveState>,
) {
    if let ReaderState::Ready(current) = &mut *state.write() {
        if let Some(page) = current.page_map.page_for_path(path) {
            current
                .navigation
                .jump_to(page, current.page_map.pages.len());
            persist_current(current, csrf, generation, in_flight, save_state);
        }
    }
}

fn activate_link(
    mut state: Signal<ReaderState>,
    link: ReadingLink,
    csrf: Signal<String>,
    generation: Signal<u64>,
    in_flight: Signal<bool>,
    save_state: Signal<SaveState>,
) {
    if link.kind == ReadingLinkKind::External {
        if let Some(url) = safe_external_url(&link) {
            if let Some(window) = web_sys::window() {
                let _ = window.open_with_url_and_target_and_features(
                    &url,
                    "_blank",
                    "noopener,noreferrer",
                );
            }
        }
        return;
    }
    if let ReaderState::Ready(current) = &mut *state.write() {
        if link.kind == ReadingLinkKind::Footnote {
            current.footnote = Some(link);
        } else if let Some(page) = current.page_map.page_for_path(&link.target_path) {
            current
                .navigation
                .jump_to(page, current.page_map.pages.len());
            persist_current(current, csrf, generation, in_flight, save_state);
        }
    }
}

fn update_settings(
    mut state: Signal<ReaderState>,
    csrf: Signal<String>,
    mut generation: Signal<u64>,
    mut in_flight: Signal<bool>,
    save_state: Signal<SaveState>,
    update: impl FnOnce(&mut ReaderSettings),
) {
    if let ReaderState::Ready(current) = &mut *state.write() {
        let current_boundary = current
            .page_map
            .pages
            .get(current.navigation.current())
            .map(|page| page.start.clone());
        update(&mut current.settings);
        current.settings = current.settings.normalized();
        if let Ok(page_map) = browser_page_map(&current.plan, current.settings) {
            let restored = current_boundary
                .as_ref()
                .and_then(|boundary| {
                    page_map.page_for_boundary(&boundary.node_path, boundary.offset)
                })
                .unwrap_or_default();
            current.page_map = page_map;
            current
                .navigation
                .move_to(restored, current.page_map.pages.len());
        }
        let settings = current.settings;
        let csrf_token = csrf.read().clone();
        let next_generation = generation().saturating_add(1);
        generation.set(next_generation);
        begin_save(save_state, "settings", "настройки");
        spawn(async move {
            browser_delay(180).await;
            while generation() == next_generation && in_flight() {
                browser_delay(40).await;
            }
            if generation() != next_generation {
                finish_save(save_state, "settings", Ok(()));
                return;
            }
            in_flight.set(true);
            let result = save_settings(settings, &csrf_token).await;
            in_flight.set(false);
            if generation() == next_generation {
                finish_save(save_state, "settings", result);
            } else {
                finish_save(save_state, "settings", Ok(()));
            }
        });
    }
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

fn persist_current(
    current: &ReaderView,
    csrf: Signal<String>,
    mut generation: Signal<u64>,
    mut in_flight: Signal<bool>,
    save_state: Signal<SaveState>,
) {
    let Some(page) = current.page_map.pages.get(current.navigation.current()) else {
        return;
    };
    let Some(block) = current.plan.block(&page.start.node_path) else {
        return;
    };
    let mut locator = block.anchor.clone();
    locator.text_range = Some(TextRange {
        start: page.start.offset,
        end: page.start.offset,
    });
    locator.quote.clear();
    let page_count = current.page_map.pages.len().max(1);
    let command = MoveReadingPositionCommand {
        material_id: current.entry.id,
        revision_id: current.document.revision_id,
        locator,
        progress_fraction: (current.navigation.current() + 1) as f32 / page_count as f32,
    };
    let csrf_token = csrf.read().clone();
    let next_generation = generation().saturating_add(1);
    generation.set(next_generation);
    begin_save(save_state, "progress", "позицию");
    spawn(async move {
        browser_delay(120).await;
        while generation() == next_generation && in_flight() {
            browser_delay(40).await;
        }
        if generation() != next_generation {
            finish_save(save_state, "progress", Ok(()));
            return;
        }
        in_flight.set(true);
        let result = save_progress(command, &csrf_token).await;
        in_flight.set(false);
        if generation() == next_generation {
            finish_save(save_state, "progress", result);
        } else {
            finish_save(save_state, "progress", Ok(()));
        }
    });
}

async fn load_reader(
    material_id: Uuid,
) -> Result<
    (
        LibraryEntry,
        ReadingDocument,
        ReaderSettings,
        Option<ReadingProgress>,
        Vec<Annotation>,
    ),
    String,
> {
    let entry: LibraryEntry = get_json(&format!("/materials/{material_id}")).await?;
    let revision_id = entry
        .active_revision_id
        .ok_or_else(|| "У материала ещё нет готовой версии для чтения.".to_owned())?;
    let document = get_json(&format!("/revisions/{revision_id}/reading-document")).await?;
    let settings = get_json("/reader/settings").await?;
    let progress = get_json(&format!("/materials/{material_id}/progress")).await?;
    let annotations = get_json(&format!("/materials/{material_id}/annotations")).await?;
    Ok((entry, document, settings, progress, annotations))
}

async fn get_json<T: for<'de> serde::Deserialize<'de>>(path: &str) -> Result<T, String> {
    let response = Request::get(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(|error| format!("Сеть/API недоступны: {error}"))?;
    if !response.ok() {
        if response.status() == 401 {
            super::account::notify_session_expired();
        }
        return Err(format!("Lumi API вернул HTTP {}.", response.status()));
    }
    response
        .json()
        .await
        .map_err(|error| format!("Некорректный ответ reader API: {error}"))
}

async fn save_settings(settings: ReaderSettings, csrf: &str) -> Result<(), String> {
    let request = Request::put(&format!("{API_BASE}/reader/settings"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .header("Idempotency-Key", &Uuid::now_v7().to_string())
        .json(&UpdateReaderSettingsCommand { settings })
        .map_err(|error| error.to_string())?;
    let response = request.send().await.map_err(|error| error.to_string())?;
    if response.status() == 401 {
        super::account::notify_session_expired();
    }
    response
        .ok()
        .then_some(())
        .ok_or_else(|| format!("Настройки не сохранены: HTTP {}", response.status()))
}

async fn save_progress(command: MoveReadingPositionCommand, csrf: &str) -> Result<(), String> {
    let request = Request::put(&format!(
        "{API_BASE}/materials/{}/progress",
        command.material_id
    ))
    .credentials(RequestCredentials::Include)
    .header("X-Lumi-CSRF", csrf)
    .header("Idempotency-Key", &Uuid::now_v7().to_string())
    .json(&command)
    .map_err(|error| error.to_string())?;
    let response = request.send().await.map_err(|error| error.to_string())?;
    if response.status() == 401 {
        super::account::notify_session_expired();
    }
    response
        .ok()
        .then_some(())
        .ok_or_else(|| format!("Позиция не сохранена: HTTP {}", response.status()))
}

fn browser_page_map(plan: &RenderPlan, settings: ReaderSettings) -> Result<PageMap, String> {
    let window = web_sys::window().ok_or_else(|| "Browser window недоступен.".to_owned())?;
    let viewport = window
        .inner_width()
        .map_err(|_| "Не удалось измерить viewport.".to_owned())?
        .as_f64()
        .unwrap_or(1024.0);
    let width: f64 = match settings.width {
        ReaderWidth::Narrow => 560.0_f64,
        ReaderWidth::Balanced => 680.0_f64,
        ReaderWidth::Wide => 820.0_f64,
    }
    .min((viewport - 32.0).max(300.0));
    let height = if viewport < 720.0 { 520.0 } else { 680.0 };
    let layout_key = format!(
        "{}:{:.0}x{:.0}:{}:browser-page-map-v1",
        plan.revision_id,
        width,
        height,
        settings.cache_key()
    );
    if let Some(cached) = PAGE_MAP_CACHE.with(|cache| cache.borrow().get(&layout_key).cloned()) {
        return Ok(cached);
    }
    let document = window
        .document()
        .ok_or_else(|| "Browser document недоступен.".to_owned())?;
    let body = document
        .body()
        .ok_or_else(|| "Browser body недоступен.".to_owned())?;
    let page = document
        .create_element("div")
        .map_err(|_| "Не удалось создать measurement page.".to_owned())?
        .dyn_into::<HtmlElement>()
        .map_err(|_| "Measurement element несовместим с HTML.".to_owned())?;
    let style = page.style();
    for (name, value) in [
        ("position", "fixed".to_owned()),
        ("visibility", "hidden".to_owned()),
        ("pointer-events", "none".to_owned()),
        ("left", "-10000px".to_owned()),
        ("top", "0".to_owned()),
        ("overflow", "hidden".to_owned()),
        ("box-sizing", "border-box".to_owned()),
        ("width", format!("{width}px")),
        ("height", format!("{height}px")),
        ("padding", "42px 48px".to_owned()),
        (
            "font-family",
            "Georgia, 'Times New Roman', serif".to_owned(),
        ),
        ("font-size", format!("{}px", settings.font_size_px)),
        ("line-height", format!("{}%", settings.line_height_percent)),
    ] {
        style
            .set_property(name, &value)
            .map_err(|_| "Не удалось настроить measurement page.".to_owned())?;
    }
    body.append_child(&page)
        .map_err(|_| "Не удалось смонтировать measurement page.".to_owned())?;

    let measured = measure_blocks(plan, &document, &page, &layout_key);
    page.remove();
    let page_map = measured?;
    page_map.validate(plan).map_err(|error| error.to_string())?;
    PAGE_MAP_CACHE.with(|cache| {
        cache.borrow_mut().insert(layout_key, page_map.clone());
    });
    Ok(page_map)
}

fn measure_blocks(
    plan: &RenderPlan,
    document: &web_sys::Document,
    page: &HtmlElement,
    layout_key: &str,
) -> Result<PageMap, String> {
    let mut pages = Vec::new();
    let mut fragments = Vec::new();
    for block in &plan.blocks {
        let text = block.text.as_deref().unwrap_or_default();
        let end = text.chars().count().max(1);
        let mut start = 0;
        while start < end {
            let whole = measurement_block(document, block, text, start, end)?;
            page.append_child(&whole)
                .map_err(|_| "Не удалось измерить reader block.".to_owned())?;
            if page_fits(page) {
                fragments.push(PageFragment {
                    node_path: block.node_path.clone(),
                    range: TextRange { start, end },
                });
                start = end;
                continue;
            }
            whole.remove();
            if block.atomic {
                if !fragments.is_empty() {
                    finish_page(&mut pages, &mut fragments, page);
                    continue;
                }
                page.append_child(&whole)
                    .map_err(|_| "Не удалось разместить atomic reader block.".to_owned())?;
                fragments.push(PageFragment {
                    node_path: block.node_path.clone(),
                    range: TextRange { start: 0, end: 1 },
                });
                start = end;
                continue;
            }
            let mut low = start + 1;
            let mut high = end;
            let mut accepted = start;
            while low <= high {
                let middle = low + (high - low) / 2;
                let probe = measurement_block(document, block, text, start, middle)?;
                page.append_child(&probe)
                    .map_err(|_| "Не удалось измерить text range.".to_owned())?;
                let fits = page_fits(page);
                probe.remove();
                if fits {
                    accepted = middle;
                    low = middle + 1;
                } else {
                    high = middle.saturating_sub(1);
                }
            }
            if accepted == start && !fragments.is_empty() {
                finish_page(&mut pages, &mut fragments, page);
                continue;
            }
            accepted = accepted.max(start + 1).min(end);
            let part = measurement_block(document, block, text, start, accepted)?;
            page.append_child(&part)
                .map_err(|_| "Не удалось разместить text range.".to_owned())?;
            fragments.push(PageFragment {
                node_path: block.node_path.clone(),
                range: TextRange {
                    start,
                    end: accepted,
                },
            });
            start = accepted;
            if start < end {
                finish_page(&mut pages, &mut fragments, page);
            }
        }
    }
    if !fragments.is_empty() {
        finish_page(&mut pages, &mut fragments, page);
    }
    Ok(PageMap {
        revision_id: plan.revision_id,
        layout_key: layout_key.to_owned(),
        pages,
    })
}

fn measurement_block(
    document: &web_sys::Document,
    block: &RenderBlock,
    text: &str,
    start: usize,
    end: usize,
) -> Result<web_sys::Element, String> {
    let tag = match block.kind {
        lumi_core::ReadingNodeKind::Heading { level: 1 } => "h2",
        lumi_core::ReadingNodeKind::Heading { level: 2 } => "h3",
        lumi_core::ReadingNodeKind::Heading { level: 3 } => "h4",
        lumi_core::ReadingNodeKind::Heading { level: 4 } => "h5",
        lumi_core::ReadingNodeKind::Heading { .. } => "h6",
        lumi_core::ReadingNodeKind::Blockquote => "blockquote",
        lumi_core::ReadingNodeKind::CodeBlock => "pre",
        lumi_core::ReadingNodeKind::Footnote => "aside",
        lumi_core::ReadingNodeKind::HorizontalRule => "hr",
        _ => "p",
    };
    let element = document
        .create_element(tag)
        .map_err(|_| "Не удалось создать measurement block.".to_owned())?;
    if !text.is_empty() {
        let source = document.create_text_node(text);
        let range = document
            .create_range()
            .map_err(|_| "Browser Range недоступен для pagination.".to_owned())?;
        range
            .set_start(&source, utf16_offset(text, start))
            .map_err(|_| "Не удалось установить начало browser Range.".to_owned())?;
        range
            .set_end(&source, utf16_offset(text, end))
            .map_err(|_| "Не удалось установить конец browser Range.".to_owned())?;
        let fragment = range
            .clone_contents()
            .map_err(|_| "Не удалось клонировать browser Range.".to_owned())?;
        element
            .append_child(&fragment)
            .map_err(|_| "Не удалось измерить browser Range.".to_owned())?;
    }
    if block.atomic {
        element
            .set_attribute(
                "style",
                "min-height: 180px; break-inside: avoid; overflow: auto",
            )
            .map_err(|_| "Не удалось настроить atomic block.".to_owned())?;
    } else {
        element
            .set_attribute("style", "margin: 0 0 1em")
            .map_err(|_| "Не удалось настроить text block.".to_owned())?;
    }
    Ok(element)
}

fn page_fits(page: &HtmlElement) -> bool {
    page.scroll_height() <= page.client_height() + 1
}

fn finish_page(pages: &mut Vec<ReaderPage>, fragments: &mut Vec<PageFragment>, page: &HtmlElement) {
    let Some(first) = fragments.first() else {
        return;
    };
    let Some(last) = fragments.last() else {
        return;
    };
    pages.push(ReaderPage {
        index: pages.len(),
        start: PageBoundary {
            node_path: first.node_path.clone(),
            offset: first.range.start,
        },
        end: PageBoundary {
            node_path: last.node_path.clone(),
            offset: last.range.end,
        },
        fragments: std::mem::take(fragments),
    });
    page.set_text_content(None);
}

fn scalar_slice(text: &str, start: usize, end: usize) -> String {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

fn utf16_offset(text: &str, scalar_offset: usize) -> u32 {
    text.chars()
        .take(scalar_offset)
        .map(char::len_utf16)
        .sum::<usize>()
        .min(u32::MAX as usize) as u32
}

fn ranges_intersect(left: TextRange, right: TextRange) -> bool {
    left.start < right.end && right.start < left.end
}
