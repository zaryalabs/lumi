//! Dioxus/DOM adapter for the shared Stage 4 reader contracts.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use gloo_net::http::Request;
use lumi_core::{
    AiContextAttachment, AiSourceScope, Anchor, AnchorResolution, Annotation, AnnotationBacklink,
    AnnotationId, AnnotationKind, AnnotationLink, AnnotationLinkState, AnnotationStatus,
    AnnotationTarget, AnnotationType, AudioAttachment, AudioRetentionPolicy, AudioUpload,
    CreateAnnotationCommand, CreateAudioAttachmentCommand, CreateAudioUploadCommand,
    DeleteAnnotationCommand, HighlightStyle, LibraryEntry, LinkTarget, LinkTargetType,
    MoveReadingPositionCommand, PageBoundary, PageFragment, PageMap, ReaderNavigation, ReaderPage,
    ReaderSettings, ReaderTheme, ReaderWidth, ReadingDocument, ReadingLink, ReadingLinkKind,
    ReadingProgress, RenderBlock, RenderPlan, ResolveAnnotationLinkCommand, TextRange,
    TranscriptArtifact, UpdateAnnotationCommand, UpdateReaderSettingsCommand,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use wasm_bindgen::{closure::Closure, JsCast};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Element as DomElement, HtmlElement, Node, RequestCredentials};

use super::account::API_BASE;

thread_local! {
    static PAGE_MAP_CACHE: RefCell<HashMap<String, Rc<PageMap>>> = RefCell::new(HashMap::new());
}

#[derive(Clone)]
struct ReaderView {
    entry: LibraryEntry,
    document: Rc<ReadingDocument>,
    plan: Rc<RenderPlan>,
    settings: ReaderSettings,
    page_map: Rc<PageMap>,
    navigation: ReaderNavigation,
    toc_open: bool,
    toc_query: String,
    settings_open: bool,
    notes_open: bool,
    footnote: Option<ReadingLink>,
    annotations: Vec<AnnotationItem>,
    selected_anchor: Option<Anchor>,
    draft_target: AnnotationTarget,
    note_composer_open: bool,
    voice_composer_open: bool,
    voice_recording: bool,
    voice_recorded: Option<crate::voice::RecordedAudio>,
    voice_preview_url: String,
    voice_uploading: bool,
    voice_error: Option<String>,
    note_title_draft: String,
    note_draft: String,
    note_tags_draft: String,
    edit_note_draft: String,
    edit_note_title: String,
    edit_note_tags: String,
    editing_note: Option<AnnotationId>,
    notes_filter: NotesFilter,
    annotation_links: HashMap<AnnotationId, Vec<AnnotationLink>>,
    annotation_backlinks: HashMap<AnnotationId, Vec<AnnotationBacklink>>,
    transcripts: HashMap<Uuid, TranscriptArtifact>,
    link_suggestions: Vec<LinkTarget>,
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

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum NotesFilter {
    #[default]
    All,
    Notes,
    Highlights,
    Voice,
}

/// API-backed reader route for one ready material.
#[component]
pub(crate) fn ReaderApp(
    material_id: Uuid,
    initial_anchor: Option<String>,
    csrf_token: String,
    on_close: EventHandler<()>,
    on_open_learning_session: EventHandler<Uuid>,
    on_manage_learning: EventHandler<(Uuid, Uuid)>,
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
        let Some(window) = web_sys::window() else {
            return;
        };
        let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            if let ReaderState::Ready(view) = &mut *state.write() {
                apply_ai_reader_target(view);
            }
        });
        let _ = window.add_event_listener_with_callback(
            crate::ai::READER_TARGET_EVENT,
            handler.as_ref().unchecked_ref(),
        );
        handler.forget();
    });
    use_effect(move || {
        let _ = reload_generation();
        state.set(ReaderState::Loading);
        let initial_anchor = initial_anchor.clone();
        spawn(async move {
            match load_reader(material_id).await {
                Ok((
                    entry,
                    document,
                    settings,
                    progress,
                    annotations,
                    links,
                    backlinks,
                    transcripts,
                )) => {
                    set_document_title(&format!("{} — Lumi", entry.display_title()));
                    let plan = Rc::new(RenderPlan::from_document(&document));
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
                            let mut view = ReaderView {
                                entry,
                                document: Rc::new(document),
                                plan,
                                settings,
                                page_map,
                                navigation,
                                toc_open: false,
                                toc_query: String::new(),
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
                                draft_target: AnnotationTarget::TextRange,
                                note_composer_open: false,
                                voice_composer_open: false,
                                voice_recording: false,
                                voice_recorded: None,
                                voice_preview_url: String::new(),
                                voice_uploading: false,
                                voice_error: None,
                                note_title_draft: String::new(),
                                note_draft: String::new(),
                                note_tags_draft: String::new(),
                                edit_note_draft: String::new(),
                                edit_note_title: String::new(),
                                edit_note_tags: String::new(),
                                editing_note: None,
                                notes_filter: NotesFilter::All,
                                annotation_links: links.into_iter().fold(
                                    HashMap::new(),
                                    |mut grouped, link| {
                                        grouped
                                            .entry(link.source_annotation_id)
                                            .or_default()
                                            .push(link);
                                        grouped
                                    },
                                ),
                                annotation_backlinks: backlinks,
                                transcripts,
                                link_suggestions: Vec::new(),
                                conflict_draft: None,
                                annotation_message: None,
                            };
                            if let Some(anchor_id) = initial_anchor.as_deref() {
                                if let Some(block) = view
                                    .plan
                                    .blocks
                                    .iter()
                                    .find(|block| block.node_id == anchor_id)
                                {
                                    if let Some(page) =
                                        view.page_map.page_for_boundary(&block.node_path, 0)
                                    {
                                        view.navigation.jump_to(page, view.page_map.pages.len());
                                        view.annotation_message =
                                            Some("Открыт результат поиска.".to_owned());
                                    }
                                }
                            }
                            apply_ai_reader_target(&mut view);
                            state.set(ReaderState::Ready(Box::new(view)));
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
            main { id: "main-content", class: "reader-loading", aria_label: "Ошибка чтения",
                p { class: "eyebrow", "Материал недоступен" }
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
            let current_unit = page
                .as_ref()
                .and_then(|page| page_unit_id(&view.document, page));
            let next_unit = view
                .page_map
                .pages
                .get(current_page.saturating_add(1))
                .and_then(|page| page_unit_id(&view.document, page));
            let chapter_end = current_unit.is_some() && current_unit != next_unit;
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
            let page_height = browser_page_dimensions(view.settings)
                .map(|(_, height)| height)
                .unwrap_or(680.0);
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
            let reader_overlay_open = view.toc_open || view.settings_open || view.notes_open;
            rsx! {
                main {
                    id: "main-content",
                    class: "reader-workspace {theme_class}",
                    aria_label: "Чтение {title}",
                    style: "--reader-font-size: {view.settings.font_size_px}px; --reader-line-height: {view.settings.line_height_percent}%; --reader-page-height: {page_height}px; --reader-progress: {((current_page + 1) * 100) / page_count.max(1)}%;",
                    onkeydown: move |event| if event.key() == Key::Escape { close_reader_overlay(state); },
                    header { class: "reader-topbar",
                        button { class: "reader-back", r#type: "button", aria_label: "Вернуться в библиотеку", title: "Библиотека", onclick: move |_| on_close.call(()),
                            span { aria_hidden: "true", "←" }
                            span { class: "reader-back-label", "Библиотека" }
                        }
                        div { class: "reader-title",
                            h1 { "{title}" }
                            span { "{creators}" }
                        }
                        div { class: "reader-tools", role: "toolbar", aria_label: "Инструменты чтения",
                            crate::search_ui::ReaderSearch { material_id }
                            button { id: "reader-toc-button", r#type: "button", aria_expanded: view.toc_open, aria_controls: "reader-toc-panel", onclick: move |_| toggle_reader_panel(state, ReaderPanel::Toc), "Оглавление" }
                            button { id: "reader-settings-button", r#type: "button", aria_expanded: view.settings_open, aria_controls: "reader-settings-panel", onclick: move |_| toggle_reader_panel(state, ReaderPanel::Settings), "Настройки" }
                            button { id: "reader-margin-note-button", r#type: "button", onclick: move |_| start_margin_note(state, current_page), "Запись на полях" }
                            button { id: "reader-voice-note-button", r#type: "button", onclick: move |_| start_margin_voice_note(state, current_page), "Голосовая заметка" }
                            button { id: "reader-notes-button", r#type: "button", aria_expanded: view.notes_open, aria_controls: "reader-notes-panel", onclick: move |_| {
                                toggle_reader_panel(state, ReaderPanel::Notes);
                            }, "Заметки ({view.annotations.len()})" }
                            details { class: "reader-more",
                                summary {
                                    role: "button",
                                    aria_label: "Дополнительные действия",
                                    "Ещё"
                                }
                                div { class: "reader-more-menu",
                                    button { r#type: "button", onclick: move |_| export_annotations(state, export_material_id), "Экспорт заметок" }
                                    crate::ai::SummaryAction {
                                        material_id: view.entry.id,
                                        revision_id: view.document.revision_id,
                                        scope_kind: lumi_core::SummaryScopeKind::Material,
                                        scope_ref: "material".to_owned(),
                                        label: "Саммари материала".to_owned(),
                                        csrf_token: csrf.read().clone(),
                                    }
                                }
                            }
                        }
                        span { class: "reader-save-state {save_class}", role: "status", aria_live: "polite", "{save_label}" }
                        div { class: "reader-chapter-progress", aria_hidden: "true", span {} }
                    }
                    if let Some(message) = view.annotation_message.clone() {
                        div { class: "reader-global-status", role: if message == "Экспорт подготовлен" { "status" } else { "alert" }, aria_live: if message == "Экспорт подготовлен" { "polite" } else { "assertive" },
                            span { "{message}" }
                            button { r#type: "button", aria_label: "Закрыть сообщение", onclick: move |_| if let ReaderState::Ready(current) = &mut *state.write() { current.annotation_message = None; }, "×" }
                        }
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
                                label { class: "toc-search",
                                    span { "Поиск по оглавлению" }
                                    input {
                                        r#type: "search",
                                        value: "{view.toc_query}",
                                        placeholder: "Название главы…",
                                        oninput: move |event| {
                                            if let ReaderState::Ready(current) = &mut *state.write() {
                                                current.toc_query = event.value();
                                            }
                                        }
                                    }
                                }
                                ol {
                                    for item in view.document.navigation.clone() {
                                        if view.toc_query.trim().is_empty() || item.label.to_lowercase().contains(&view.toc_query.trim().to_lowercase()) {
                                            li { button {
                                                class: if view.page_map.page_for_path(&item.target_path) == Some(current_page) { "current" } else { "" },
                                                aria_current: if view.page_map.page_for_path(&item.target_path) == Some(current_page) { "location" } else { "false" },
                                                r#type: "button",
                                                onclick: move |_| jump_to_path(state, &item.target_path, csrf, progress_generation, progress_in_flight, save_state),
                                                "{item.label}"
                                            } }
                                        }
                                    }
                                }
                                button { class: "focus-sentinel", r#type: "button", aria_label: "Вернуться в начало панели", onfocus: move |_| focus_drawer_edge("reader-toc-panel", true) }
                            }
                        }

                        section { class: "reader-stage {width_class}", aria_label: "Страница книги", aria_hidden: reader_overlay_open, inert: reader_overlay_open.then_some(true),
                            if view.navigation.can_go_back() || view.navigation.can_go_forward() {
                                div { class: "reader-history", role: "toolbar", aria_label: "История переходов",
                                    button { r#type: "button", aria_label: "Назад по истории", disabled: !view.navigation.can_go_back(), onclick: move |_| {
                                        if let ReaderState::Ready(current) = &mut *state.write() { current.navigation.go_back(); persist_current(current, csrf, progress_generation, progress_in_flight, save_state); }
                                        reset_reader_page_view();
                                    }, "↶" }
                                    button { r#type: "button", aria_label: "Вперёд по истории", disabled: !view.navigation.can_go_forward(), onclick: move |_| {
                                        if let ReaderState::Ready(current) = &mut *state.write() { current.navigation.go_forward(); persist_current(current, csrf, progress_generation, progress_in_flight, save_state); }
                                        reset_reader_page_view();
                                    }, "↷" }
                                }
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
                            if chapter_end {
                                if let Some(scope_ref) = current_unit.clone() {
                                    div { class: "reader-summary-action", role: "region", aria_label: "Саммари главы",
                                        crate::ai::SummaryAction {
                                            material_id: view.entry.id,
                                            revision_id: view.document.revision_id,
                                            scope_kind: lumi_core::SummaryScopeKind::Chapter,
                                            scope_ref: scope_ref.clone(),
                                            label: "Создать или открыть саммари главы".to_owned(),
                                            csrf_token: csrf.read().clone(),
                                        }
                                    }
                                    if let Some(progress) = current_progress_command(&view) {
                                        crate::learning::CompletionOffer {
                                            key: "{view.document.revision_id}:{scope_ref}",
                                            progress,
                                            completion: lumi_core::CompleteReadingScopeCommand {
                                                material_id: view.entry.id,
                                                revision_id: view.document.revision_id,
                                                scope_kind: lumi_core::LearningScopeKind::ContentUnit,
                                                content_unit_id: Some(scope_ref),
                                                anchor: None,
                                                trigger: lumi_core::ReadingCompletionTrigger::ReaderBoundary,
                                            },
                                            csrf_token: csrf.read().clone(),
                                            on_open_session: on_open_learning_session,
                                            on_manage_items: on_manage_learning,
                                        }
                                    }
                                }
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
                                    input { r#type: "range", min: "15", max: "30", value: "{view.settings.font_size_px}", aria_label: "Размер текста", onchange: move |event| {
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
                        SelectionComposer {
                            state,
                            csrf,
                            save_state,
                            anchor,
                            target: view.draft_target.clone(),
                            title: view.note_title_draft.clone(),
                            draft: view.note_draft.clone(),
                            tags: view.note_tags_draft.clone(),
                            note_composer_open: view.note_composer_open,
                            voice_composer_open: view.voice_composer_open,
                            voice_recording: view.voice_recording,
                            voice_preview_url: view.voice_preview_url.clone(),
                            voice_uploading: view.voice_uploading,
                            voice_error: view.voice_error.clone(),
                        }
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
    plan: Rc<RenderPlan>,
    annotations: Vec<AnnotationItem>,
    on_link: EventHandler<ReadingLink>,
) -> Element {
    let segments = annotation_segments(&block, range, &plan, &annotations);
    let continued = range.start > 0;
    let content = rsx! {
        if continued { span { class: "continued-marker", aria_hidden: "true", "…" } }
        for segment in segments {
            if let Some(link_index) = segment.link_index {
                if let Some(link) = block.links.get(link_index).cloned() {
                    if link.kind == ReadingLinkKind::External {
                        if let Some(url) = safe_external_url(&link) {
                            a {
                                class: "inline-link {segment.class_name}",
                                href: "{url}",
                                target: "_blank",
                                rel: "noopener noreferrer",
                                "data-reader-source": "true",
                                "data-node-id": "{block.node_id}",
                                "data-scalar-start": "{segment.scalar_start}",
                                "{segment.text}"
                            }
                        } else {
                            span {
                                class: "source-text {segment.class_name}",
                                "data-reader-source": "true",
                                "data-node-id": "{block.node_id}",
                                "data-scalar-start": "{segment.scalar_start}",
                                "{segment.text}"
                            }
                        }
                    } else {
                        button {
                            class: "inline-link {segment.class_name}",
                            r#type: "button",
                            "data-reader-source": "true",
                            "data-node-id": "{block.node_id}",
                            "data-scalar-start": "{segment.scalar_start}",
                            onclick: move |_| on_link.call(link.clone()),
                            "{segment.text}"
                        }
                    }
                }
            } else {
                span {
                    class: "source-text {segment.class_name}",
                    "data-reader-source": "true",
                    "data-node-id": "{block.node_id}",
                    "data-scalar-start": "{segment.scalar_start}",
                    "{segment.text}"
                }
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
            rsx! { figure { id: "{block.node_id}", if let Some(src) = src { img { src, alt: "{alt}", loading: "lazy", width: "1200", height: "800" } } else { div { class: "image-placeholder", "{alt}" } } } }
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
    class_name: String,
    link_index: Option<usize>,
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
    let mut classes = vec![String::new(); chars.len()];
    let mut link_indices = vec![None; chars.len()];
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
            AnnotationKind::Highlight {
                style: HighlightStyle::Bold,
            } => "annotation-highlight-bold",
            AnnotationKind::Highlight {
                style: HighlightStyle::Green,
            } => "annotation-highlight-green",
            AnnotationKind::Highlight {
                style: HighlightStyle::Blue,
            } => "annotation-highlight-blue",
            AnnotationKind::Highlight {
                style: HighlightStyle::Yellow,
            } => "annotation-highlight",
            AnnotationKind::Note { .. } => "annotation-note",
            AnnotationKind::VoiceNote { .. } => "annotation-voice",
        };
        for scalar in start..end {
            if let Some(value) = classes.get_mut(scalar.saturating_sub(visible.start)) {
                if !value
                    .split_ascii_whitespace()
                    .any(|class| class == class_name)
                {
                    if !value.is_empty() {
                        value.push(' ');
                    }
                    value.push_str(class_name);
                }
            }
        }
    }
    for (link_index, link) in block.links.iter().enumerate() {
        let start = link.text_range.start.max(visible.start);
        let end = link.text_range.end.min(visible.end);
        for scalar in start..end {
            if let Some(value) = link_indices.get_mut(scalar.saturating_sub(visible.start)) {
                *value = Some(link_index);
            }
        }
    }
    let mut output = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let class_name = classes[start].clone();
        let link_index = link_indices[start];
        let mut end = start + 1;
        while end < chars.len() && classes[end] == class_name && link_indices[end] == link_index {
            end += 1;
        }
        output.push(TextSegment {
            text: chars[start..end].iter().collect(),
            scalar_start: visible.start + start,
            class_name,
            link_index,
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
    let visible_annotations = view
        .annotations
        .iter()
        .filter(|item| match view.notes_filter {
            NotesFilter::All => true,
            NotesFilter::Notes => matches!(
                item.annotation.annotation_type,
                AnnotationType::Note | AnnotationType::MarginNote
            ),
            NotesFilter::Highlights => item.annotation.annotation_type == AnnotationType::Highlight,
            NotesFilter::Voice => item.annotation.annotation_type == AnnotationType::VoiceNote,
        })
        .cloned()
        .collect::<Vec<_>>();
    rsx! {
        aside { id: "reader-notes-panel", class: "reader-drawer notes-drawer", tabindex: "-1", aria_label: "Личные заметки и выделения", onkeydown: move |event| {
            if event.key() == Key::Escape { close_reader_panel(state, ReaderPanel::Notes); }
        },
            button { class: "focus-sentinel", r#type: "button", aria_label: "Перейти в конец панели", onfocus: move |_| focus_drawer_edge("reader-notes-panel", false) }
            div { class: "drawer-heading",
                div { h2 { "Заметки" } p { class: "private-label", "Только для вас" } }
                button { id: "reader-notes-close", r#type: "button", aria_label: "Закрыть заметки", onclick: move |_| close_reader_panel(state, ReaderPanel::Notes), "×" }
            }
            div { class: "annotation-tabs", role: "tablist", aria_label: "Тип записей",
                button { r#type: "button", role: "tab", aria_selected: view.notes_filter == NotesFilter::All, onclick: move |_| set_notes_filter(state, NotesFilter::All), "Все" }
                button { r#type: "button", role: "tab", aria_selected: view.notes_filter == NotesFilter::Notes, onclick: move |_| set_notes_filter(state, NotesFilter::Notes), "Заметки" }
                button { r#type: "button", role: "tab", aria_selected: view.notes_filter == NotesFilter::Highlights, onclick: move |_| set_notes_filter(state, NotesFilter::Highlights), "Выделения" }
                button { r#type: "button", role: "tab", aria_selected: view.notes_filter == NotesFilter::Voice, onclick: move |_| set_notes_filter(state, NotesFilter::Voice), "Голос" }
            }
            if view.annotations.is_empty() {
                p { class: "notes-empty", "Выделите фрагмент на странице, чтобы сохранить выделение или заметку." }
            } else if visible_annotations.is_empty() {
                p { class: "notes-empty", "Для этого фильтра записей пока нет." }
            } else {
                ol { class: "annotation-list",
                    for item in visible_annotations {
                        AnnotationPanelItem {
                            state,
                            csrf,
                            save_state,
                            progress_generation,
                            progress_in_flight,
                            item,
                            editing: view.editing_note,
                            draft: view.edit_note_draft.clone(),
                            edit_title: view.edit_note_title.clone(),
                            edit_tags: view.edit_note_tags.clone()
                        }
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
    target: AnnotationTarget,
    title: String,
    draft: String,
    tags: String,
    note_composer_open: bool,
    voice_composer_open: bool,
    voice_recording: bool,
    voice_preview_url: String,
    voice_uploading: bool,
    voice_error: Option<String>,
) -> Element {
    let yellow_anchor = anchor.clone();
    let bold_anchor = anchor.clone();
    let ask_anchor = anchor.clone();
    let explain_anchor = anchor.clone();
    let summary_anchor = anchor.clone();
    rsx! {
        if !note_composer_open && !voice_composer_open {
            div { class: "selection-actions", role: "toolbar", aria_label: if target.is_text_range() { "Действия с выделением" } else { "Действия с записью на полях" },
                button { class: "primary-action", r#type: "button", onclick: move |_| ask_ai_about_selection(state, ask_anchor.clone(), "", false), "Спросить ИИ" }
                button { r#type: "button", onclick: move |_| ask_ai_about_selection(
                    state,
                    explain_anchor.clone(),
                    "Объясни выделенный фрагмент простыми словами, не теряя его смысл.",
                    true,
                ), "Объясни проще" }
                button { r#type: "button", onclick: move |_| ask_ai_about_selection(
                    state,
                    summary_anchor.clone(),
                    "Кратко перескажи выделенный фрагмент и сохрани ключевые тезисы.",
                    true,
                ), "Кратко перескажи" }
                if target.is_text_range() {
                    button { r#type: "button", onclick: move |_| create_highlight(state, yellow_anchor.clone(), HighlightStyle::Yellow, csrf, save_state), "Жёлтым" }
                    button { r#type: "button", onclick: move |_| create_highlight(state, bold_anchor.clone(), HighlightStyle::Bold, csrf, save_state), "Жирным" }
                }
                button { r#type: "button", onclick: move |_| {
                    if let ReaderState::Ready(current) = &mut *state.write() {
                        current.note_title_draft.clear();
                        current.note_draft.clear();
                        current.note_tags_draft.clear();
                        current.note_composer_open = true;
                        current.annotation_message = None;
                    }
                    defer_reader_focus("reader-note-draft");
                }, "Заметка" }
                button { r#type: "button", onclick: move |_| open_voice_composer(state), "Голос" }
                button { r#type: "button", onclick: move |_| dismiss_selection(state), "Отмена" }
            }
        } else if note_composer_open {
            form { class: "note-composer", onsubmit: move |event| { event.prevent_default(); create_note(state, anchor.clone(), csrf, save_state); },
                p { class: "annotation-kind", if target.is_text_range() { "Заметка к выделению" } else { "Запись на полях" } }
                label { "Заголовок (необязательно)", input { name: "note_title", autocomplete: "off", maxlength: "240", placeholder: "Короткое название", value: "{title}", oninput: move |event| if let ReaderState::Ready(current) = &mut *state.write() { current.note_title_draft = event.value(); } } }
                label { "Текст заметки", textarea { id: "reader-note-draft", name: "note_body", autocomplete: "off", placeholder: "Добавьте мысль…", value: "{draft}", oninput: move |event| update_note_draft(state, event.value(), false) } }
                WikilinkSuggestions { state, draft: draft.clone() }
                label { "Теги", input { name: "note_tags", autocomplete: "off", placeholder: "чтение, идея", value: "{tags}", oninput: move |event| if let ReaderState::Ready(current) = &mut *state.write() { current.note_tags_draft = event.value(); } } }
                div { class: "dialog-actions",
                    button { class: "secondary-action", r#type: "button", onclick: move |_| dismiss_selection(state), "Отмена" }
                    button { class: "primary-action", r#type: "submit", disabled: draft.trim().is_empty(), "Сохранить заметку" }
                }
            }
        } else {
            div { class: "voice-note-composer", role: "dialog", aria_label: "Новая голосовая заметка",
                p { class: "annotation-kind", if target.is_text_range() { "Голосовая заметка к выделению" } else { "Голосовая заметка на полях" } }
                if voice_recording {
                    p { role: "status", "Идёт запись…" }
                    button { class: "primary-action", r#type: "button", onclick: move |_| finish_voice_note_recording(state), "Остановить запись" }
                } else if !voice_preview_url.is_empty() {
                    audio { controls: true, src: "{voice_preview_url}", aria_label: "Предпрослушивание голосовой заметки" }
                    div { class: "dialog-actions",
                        button { class: "secondary-action", r#type: "button", disabled: voice_uploading, onclick: move |_| discard_voice_note_recording(state), "Удалить запись" }
                        button { class: "primary-action", r#type: "button", disabled: voice_uploading, onclick: move |_| upload_voice_note(state, csrf, save_state), if voice_uploading { "Загружаем…" } else { "Сохранить голосовую заметку" } }
                    }
                } else {
                    button { class: "primary-action", r#type: "button", onclick: move |_| begin_voice_note_recording(state), "Начать запись" }
                    label { class: "voice-file-fallback",
                        "Или выберите аудиофайл"
                        input {
                            r#type: "file",
                            accept: ".webm,.ogg,.oga,.m4a,.mp4,.mp3,.wav,audio/webm,audio/ogg,audio/mp4,audio/mpeg,audio/wav",
                            aria_label: "Аудиофайл голосовой заметки",
                            onchange: move |event| {
                                let Some(file) = event.files().into_iter().next() else { return; };
                                spawn(async move {
                                    let name = file.name();
                                    match file.read_bytes().await {
                                        Ok(bytes) => set_voice_note_file(state, &name, bytes.to_vec()),
                                        Err(_) => set_voice_note_error(state, "Не удалось прочитать аудиофайл.".to_owned()),
                                    }
                                });
                            }
                        }
                    }
                }
                if let Some(error) = voice_error {
                    p { class: "annotation-conflict", role: "alert", "{error}" }
                }
                button { class: "secondary-action", r#type: "button", disabled: voice_uploading, onclick: move |_| dismiss_selection(state), "Отмена" }
            }
        }
    }
}

#[component]
fn WikilinkSuggestions(state: Signal<ReaderState>, draft: String) -> Element {
    let suggestions = match &*state.read() {
        ReaderState::Ready(view) => view.link_suggestions.clone(),
        _ => Vec::new(),
    };
    if suggestions.is_empty() || unfinished_wikilink_query(&draft).is_none() {
        return rsx! {};
    }
    rsx! {
        div { class: "wikilink-suggestions", role: "listbox", aria_label: "Подсказки для внутренней ссылки",
            for suggestion in suggestions {
                button {
                    r#type: "button",
                    role: "option",
                    onclick: move |_| insert_wikilink_suggestion(state, suggestion.display_path.clone()),
                    "{suggestion.display_path}"
                }
            }
        }
    }
}

fn update_note_draft(mut state: Signal<ReaderState>, value: String, editing: bool) {
    let (query, material_id) = {
        let ReaderState::Ready(view) = &mut *state.write() else {
            return;
        };
        if editing {
            view.edit_note_draft = value.clone();
        } else {
            view.note_draft = value.clone();
        }
        (unfinished_wikilink_query(&value), view.entry.id)
    };
    let Some(query) = query else {
        if let ReaderState::Ready(view) = &mut *state.write() {
            view.link_suggestions.clear();
        }
        return;
    };
    spawn(async move {
        let path = format!(
            "/links/suggest?q={}&material_id={material_id}",
            js_sys::encode_uri_component(&query)
        );
        match get_json::<Vec<LinkTarget>>(&path).await {
            Ok(suggestions) => {
                if let ReaderState::Ready(view) = &mut *state.write() {
                    let active = if editing {
                        &view.edit_note_draft
                    } else {
                        &view.note_draft
                    };
                    if unfinished_wikilink_query(active).as_deref() == Some(query.as_str()) {
                        view.link_suggestions = suggestions;
                    }
                }
            }
            Err(_) => {
                if let ReaderState::Ready(view) = &mut *state.write() {
                    view.link_suggestions.clear();
                }
            }
        }
    });
}

fn unfinished_wikilink_query(value: &str) -> Option<String> {
    let start = value.rfind("[[")?;
    let tail = &value[start + 2..];
    if tail.contains("]]") || tail.contains('\n') || tail.len() > 1_000 {
        return None;
    }
    let query = tail.trim();
    (!query.is_empty()).then(|| query.to_owned())
}

fn insert_wikilink_suggestion(mut state: Signal<ReaderState>, display_path: String) {
    let ReaderState::Ready(view) = &mut *state.write() else {
        return;
    };
    let draft = if view.editing_note.is_some() {
        &mut view.edit_note_draft
    } else {
        &mut view.note_draft
    };
    if let Some(start) = draft.rfind("[[") {
        draft.replace_range(start.., &format!("[[{display_path}]]"));
    }
    view.link_suggestions.clear();
}

fn open_voice_composer(mut state: Signal<ReaderState>) {
    if let ReaderState::Ready(view) = &mut *state.write() {
        view.note_composer_open = false;
        view.voice_composer_open = true;
        view.voice_error = None;
    }
}

fn begin_voice_note_recording(mut state: Signal<ReaderState>) {
    if let ReaderState::Ready(view) = &mut *state.write() {
        view.voice_error = None;
    }
    spawn(async move {
        match crate::voice::begin_recording().await {
            Ok(()) => {
                if let ReaderState::Ready(view) = &mut *state.write() {
                    view.voice_recording = true;
                }
            }
            Err(error) => set_voice_note_error(state, error),
        }
    });
}

fn finish_voice_note_recording(mut state: Signal<ReaderState>) {
    spawn(async move {
        match crate::voice::finish_recording().await {
            Ok(recording) => set_voice_note_recording(state, recording),
            Err(error) => {
                if let ReaderState::Ready(view) = &mut *state.write() {
                    view.voice_recording = false;
                    view.voice_error = Some(error);
                }
            }
        }
    });
}

fn set_voice_note_file(state: Signal<ReaderState>, name: &str, bytes: Vec<u8>) {
    match crate::voice::RecordedAudio::from_file(name, bytes) {
        Ok(recording) => set_voice_note_recording(state, recording),
        Err(error) => set_voice_note_error(state, error),
    }
}

fn set_voice_note_recording(
    mut state: Signal<ReaderState>,
    recording: crate::voice::RecordedAudio,
) {
    let preview = crate::voice::preview_url(&recording);
    if let ReaderState::Ready(view) = &mut *state.write() {
        if !view.voice_preview_url.is_empty() {
            crate::voice::revoke_preview(&view.voice_preview_url);
        }
        view.voice_recording = false;
        view.voice_recorded = Some(recording);
        view.voice_preview_url = preview;
        view.voice_error = None;
    }
}

fn discard_voice_note_recording(mut state: Signal<ReaderState>) {
    crate::voice::cancel_recording();
    if let ReaderState::Ready(view) = &mut *state.write() {
        if !view.voice_preview_url.is_empty() {
            crate::voice::revoke_preview(&view.voice_preview_url);
        }
        view.voice_recording = false;
        view.voice_recorded = None;
        view.voice_preview_url.clear();
        view.voice_error = None;
    }
}

fn set_voice_note_error(mut state: Signal<ReaderState>, error: String) {
    if let ReaderState::Ready(view) = &mut *state.write() {
        view.voice_error = Some(error);
        view.voice_recording = false;
        view.voice_uploading = false;
    }
}

fn upload_voice_note(
    mut state: Signal<ReaderState>,
    csrf: Signal<String>,
    save_state: Signal<SaveState>,
) {
    let (recording, anchor, target, preview_url) = match &*state.read() {
        ReaderState::Ready(view) => {
            let Some(recording) = view.voice_recorded.clone() else {
                return;
            };
            let Some(anchor) = view.selected_anchor.clone() else {
                return;
            };
            (
                recording,
                anchor,
                view.draft_target.clone(),
                view.voice_preview_url.clone(),
            )
        }
        _ => return,
    };
    if let ReaderState::Ready(view) = &mut *state.write() {
        view.voice_uploading = true;
        view.voice_error = None;
    }
    let csrf_token = csrf.read().clone();
    spawn(async move {
        match upload_voice_attachment(recording, &csrf_token).await {
            Ok(attachment) => {
                crate::voice::revoke_preview(&preview_url);
                if let ReaderState::Ready(view) = &mut *state.write() {
                    view.voice_recorded = None;
                    view.voice_preview_url.clear();
                    view.voice_uploading = false;
                    view.voice_composer_open = false;
                }
                create_annotation_optimistic(
                    state,
                    anchor,
                    target,
                    AnnotationKind::VoiceNote {
                        audio_attachment_id: attachment.id,
                        transcript_artifact_id: None,
                        waveform_summary: None,
                    },
                    None,
                    Vec::new(),
                    csrf,
                    save_state,
                );
            }
            Err(error) => set_voice_note_error(state, error),
        }
    });
}

pub(crate) async fn upload_voice_attachment(
    recording: crate::voice::RecordedAudio,
    csrf: &str,
) -> Result<AudioAttachment, String> {
    let checksum = hex_sha256(&recording.bytes);
    let upload: AudioUpload = post_reader_json(
        "/blobs/uploads",
        &CreateAudioUploadCommand {
            media_type: recording.media_type.clone(),
            byte_length: recording.bytes.len() as u64,
            checksum_sha256: checksum,
        },
        csrf,
    )
    .await?;
    put_audio_bytes(
        &format!("/blobs/uploads/{}", upload.id),
        &recording.media_type,
        recording.bytes,
        csrf,
    )
    .await?;
    let _: AudioUpload =
        post_reader_empty(&format!("/blobs/uploads/{}/complete", upload.id), csrf).await?;
    post_reader_json(
        "/audio/attachments",
        &CreateAudioAttachmentCommand {
            upload_id: upload.id,
            retention: AudioRetentionPolicy::KeepUntilDeleted,
            duration_ms: recording.duration_ms,
            idempotency_key: Uuid::now_v7().to_string(),
        },
        csrf,
    )
    .await
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn ask_ai_about_selection(
    mut state: Signal<ReaderState>,
    anchor: Anchor,
    instruction: &str,
    auto_submit: bool,
) {
    let handoff = {
        let ReaderState::Ready(view) = &*state.read() else {
            return;
        };
        let label_quote = anchor.quote.chars().take(80).collect::<String>();
        crate::ai::ReaderAiHandoff {
            attachment: AiContextAttachment::Source {
                kind: "selection".to_owned(),
                material_id: view.entry.id,
                revision_id: view.document.revision_id,
                scope: AiSourceScope::Selection {
                    material_id: view.entry.id,
                    revision_id: view.document.revision_id,
                    anchor: Box::new(anchor),
                },
                display_label: format!("{} — «{label_quote}»", view.entry.display_title()),
            },
            instruction: instruction.to_owned(),
            auto_submit,
        }
    };
    match crate::ai::dispatch_reader_handoff(&handoff) {
        Ok(()) => {
            if let ReaderState::Ready(view) = &mut *state.write() {
                view.selected_anchor = None;
                view.note_composer_open = false;
                view.annotation_message = Some("Фрагмент прикреплён к AI-чату.".to_owned());
            }
        }
        Err(message) => {
            if let ReaderState::Ready(view) = &mut *state.write() {
                view.annotation_message = Some(message);
            }
        }
    }
}

fn apply_ai_reader_target(view: &mut ReaderView) {
    let Some(target) = crate::ai::take_reader_target(view.entry.id) else {
        return;
    };
    let AiContextAttachment::Source {
        kind,
        revision_id,
        scope,
        ..
    } = target
    else {
        return;
    };
    if revision_id != view.document.revision_id {
        view.annotation_message =
            Some("Источник ответа относится к другой версии материала.".to_owned());
        return;
    }
    let learning_source = kind == "learning_source";
    match scope {
        AiSourceScope::Selection { anchor, .. } => match view.plan.resolve_anchor(&anchor) {
            AnchorResolution::Resolved { anchor, .. } => {
                let offset = anchor.text_range.map_or(0, |range| range.start);
                if let Some(page) = view.page_map.page_for_boundary(&anchor.node_path, offset) {
                    view.navigation.jump_to(page, view.page_map.pages.len());
                }
                view.selected_anchor = Some(*anchor);
                view.annotation_message = Some("Открыт источник ответа AI.".to_owned());
            }
            AnchorResolution::Unresolved => {
                view.annotation_message =
                    Some("Не удалось восстановить источник ответа в этой версии.".to_owned());
            }
        },
        AiSourceScope::Chapter { scope_ref, .. } => {
            if let Some(page) =
                view.page_map.pages.iter().position(|page| {
                    page_unit_id(&view.document, page).as_ref() == Some(&scope_ref)
                })
            {
                view.navigation.jump_to(page, view.page_map.pages.len());
                view.annotation_message = Some(
                    if learning_source {
                        "Открыт источник задания."
                    } else {
                        "Открыт источник саммари."
                    }
                    .to_owned(),
                );
            } else {
                view.annotation_message = Some(
                    if learning_source {
                        "Не удалось найти раздел источника задания."
                    } else {
                        "Не удалось найти главу источника саммари."
                    }
                    .to_owned(),
                );
            }
        }
        AiSourceScope::Material { .. } => {
            view.navigation.jump_to(0, view.page_map.pages.len());
            view.annotation_message = Some(
                if learning_source {
                    "Открыт материал источника задания."
                } else {
                    "Открыт материал источника саммари."
                }
                .to_owned(),
            );
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
    edit_title: String,
    edit_tags: String,
) -> Element {
    let target_anchor = item.annotation.anchor.clone();
    let delete_value = item.annotation.clone();
    let retry_value = item.annotation.clone();
    let edit_value = item.annotation.clone();
    let yellow_value = item.annotation.clone();
    let bold_value = item.annotation.clone();
    let status_value = item.annotation.clone();
    let annotation_id = item.annotation.id;
    let quote = item.annotation.anchor.quote.clone();
    let (links, backlinks, transcript) = match &*state.read() {
        ReaderState::Ready(view) => {
            let transcript = item
                .annotation
                .kind
                .audio_attachment_id()
                .and_then(|attachment_id| view.transcripts.get(&attachment_id).cloned());
            (
                view.annotation_links
                    .get(&annotation_id)
                    .cloned()
                    .unwrap_or_default(),
                view.annotation_backlinks
                    .get(&annotation_id)
                    .cloned()
                    .unwrap_or_default(),
                transcript,
            )
        }
        _ => (Vec::new(), Vec::new(), None),
    };
    rsx! {
        li { class: "annotation-item",
            button { class: "annotation-target", r#type: "button", onclick: move |_| navigate_to_annotation(state, &target_anchor, csrf, progress_generation, progress_in_flight, save_state), blockquote { "{quote}" } }
            if let Some(title) = item.annotation.title.clone() { strong { "{title}" } }
            if !item.annotation.tags.is_empty() {
                ul { class: "annotation-tags", aria_label: "Теги",
                    for tag in item.annotation.tags.clone() { li { "{tag}" } }
                }
            }
            match item.annotation.kind.clone() {
                AnnotationKind::Highlight { style } => rsx! {
                    p { class: "annotation-kind", "Выделение · {highlight_style_label(style)}" }
                    div { class: "annotation-style-actions", role: "group", aria_label: "Стиль выделения",
                        button { r#type: "button", disabled: style == HighlightStyle::Yellow, onclick: move |_| update_highlight_style(state, yellow_value.clone(), HighlightStyle::Yellow, csrf, save_state), "Жёлтый" }
                        button { r#type: "button", disabled: style == HighlightStyle::Bold, onclick: move |_| update_highlight_style(state, bold_value.clone(), HighlightStyle::Bold, csrf, save_state), "Жирный" }
                    }
                },
                AnnotationKind::Note { body } => rsx! {
                    p { class: "annotation-kind", if item.annotation.annotation_type == AnnotationType::MarginNote { "Запись на полях" } else { "Заметка" } }
                    p { "{body}" }
                    button { r#type: "button", onclick: move |_| if let ReaderState::Ready(current) = &mut *state.write() {
                        current.editing_note = Some(annotation_id);
                        current.edit_note_draft = body.clone();
                        current.edit_note_title = item.annotation.title.clone().unwrap_or_default();
                        current.edit_note_tags = item.annotation.tags.join(", ");
                        current.conflict_draft = None;
                    }, "Изменить" }
                },
                AnnotationKind::VoiceNote { audio_attachment_id, transcript_artifact_id, .. } => rsx! {
                    p { class: "annotation-kind", "Голосовая заметка" }
                    audio {
                        controls: true,
                        preload: "metadata",
                        src: "{API_BASE}/audio/attachments/{audio_attachment_id}/audio",
                        aria_label: "Голосовая заметка",
                    }
                    if let Some(transcript) = transcript {
                        details { class: "voice-transcript",
                            summary { "Транскрипт" }
                            p { "{transcript.text}" }
                        }
                    } else if transcript_artifact_id.is_some() {
                        p { class: "annotation-kind", "Связанный транскрипт временно недоступен." }
                    }
                },
            }
            if !links.is_empty() {
                ul { class: "annotation-links", aria_label: "Ссылки из заметки",
                    for link in links {
                        li {
                            match link.state {
                                AnnotationLinkState::Resolved => rsx! {
                                    button { r#type: "button", onclick: {
                                        let target = link.target.clone();
                                        move |_| if let Some(target) = target.clone() { navigate_to_link_target(state, target, csrf, progress_generation, progress_in_flight, save_state); }
                                    }, "↗ {link.display_path}" }
                                },
                                AnnotationLinkState::Unresolved => rsx! {
                                    span { class: "link-unresolved", "Не найдена: {link.display_path}" }
                                },
                                AnnotationLinkState::Ambiguous => rsx! {
                                    details {
                                        summary { "Уточнить: {link.display_path}" }
                                        for candidate in link.candidates.clone() {
                                            button { r#type: "button", onclick: {
                                                let candidate = candidate.clone();
                                                move |_| resolve_annotation_link(state, link.id, candidate.clone(), csrf)
                                            }, "{candidate.display_path}" }
                                        }
                                    }
                                },
                            }
                        }
                    }
                }
            }
            if !backlinks.is_empty() {
                details { class: "annotation-backlinks",
                    summary { "Обратные ссылки ({backlinks.len()})" }
                    ul {
                        for backlink in backlinks {
                            li {
                                button {
                                    r#type: "button",
                                    onclick: move |_| navigate_to_backlink(
                                        state,
                                        backlink.clone(),
                                        csrf,
                                        progress_generation,
                                        progress_in_flight,
                                        save_state,
                                    ),
                                    "{backlink.source_display_path}"
                                }
                            }
                        }
                    }
                }
            }
            button {
                r#type: "button",
                onclick: move |_| toggle_annotation_status(state, status_value.clone(), csrf, save_state),
                if item.annotation.status == AnnotationStatus::Active { "Архивировать" } else { "Восстановить" }
            }
            button { class: "danger-link", r#type: "button", onclick: move |_| delete_annotation_optimistic(state, delete_value.clone(), csrf, save_state), "Удалить" }
            if item.sync_state == ItemSyncState::Saving { span { role: "status", "Сохраняем…" } }
            if item.sync_state == ItemSyncState::Failed { button { r#type: "button", onclick: move |_| retry_failed_annotation(state, retry_value.clone(), csrf, save_state), "Повторить" } }
            if item.sync_state == ItemSyncState::Conflicted { p { class: "annotation-conflict", role: "alert", "Заметка изменилась в другом окне. Ваш текст сохранён в редакторе." } }
            if editing == Some(annotation_id) {
                form { class: "note-editor", onsubmit: move |event| { event.prevent_default(); update_note_optimistic(state, edit_value.clone(), csrf, save_state); },
                    label { "Заголовок", input { name: "edited_note_title", maxlength: "240", value: "{edit_title}", oninput: move |event| if let ReaderState::Ready(current) = &mut *state.write() { current.edit_note_title = event.value(); } } }
                    label { "Редактировать заметку", textarea { name: "edited_note_body", autocomplete: "off", value: "{draft}", oninput: move |event| update_note_draft(state, event.value(), true) } }
                    WikilinkSuggestions { state, draft: draft.clone() }
                    label { "Теги", input { name: "edited_note_tags", value: "{edit_tags}", oninput: move |event| if let ReaderState::Ready(current) = &mut *state.write() { current.edit_note_tags = event.value(); } } }
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
                view.draft_target = AnnotationTarget::TextRange;
                view.annotation_message = None;
            }
            Err(error) if error != "Выделение пусто" => {
                view.annotation_message = Some("Выделите текст внутри страницы книги.".to_owned())
            }
            Err(_) => {}
        }
    }
}

fn start_margin_note(mut state: Signal<ReaderState>, page_index: usize) {
    let anchor_and_target = {
        let ReaderState::Ready(view) = &*state.read() else {
            return;
        };
        let Some(fragment) = view
            .page_map
            .pages
            .get(page_index)
            .and_then(|page| page.fragments.first())
        else {
            return;
        };
        let Some(block) = view.plan.block(&fragment.node_path) else {
            return;
        };
        let text_length = block.text.as_deref().map_or(0, |text| text.chars().count());
        let anchor = view
            .plan
            .anchor_from_selection(&fragment.node_path, 0, &fragment.node_path, text_length)
            .ok();
        anchor.map(|anchor| {
            (
                anchor,
                AnnotationTarget::Block {
                    path: fragment.node_path.clone(),
                },
            )
        })
    };
    let Some((anchor, target)) = anchor_and_target else {
        if let ReaderState::Ready(view) = &mut *state.write() {
            view.annotation_message =
                Some("На этой странице нет блока для записи на полях.".to_owned());
        }
        return;
    };
    if let ReaderState::Ready(view) = &mut *state.write() {
        view.selected_anchor = Some(anchor);
        view.draft_target = target;
        view.note_composer_open = true;
        view.note_title_draft.clear();
        view.note_draft.clear();
        view.note_tags_draft.clear();
        view.annotation_message = None;
    }
    defer_reader_focus("reader-note-draft");
}

fn start_margin_voice_note(mut state: Signal<ReaderState>, page_index: usize) {
    start_margin_note(state, page_index);
    if let ReaderState::Ready(view) = &mut *state.write() {
        if view.selected_anchor.is_some() {
            view.note_composer_open = false;
            view.voice_composer_open = true;
            view.voice_error = None;
        }
    }
}

fn set_notes_filter(mut state: Signal<ReaderState>, filter: NotesFilter) {
    if let ReaderState::Ready(view) = &mut *state.write() {
        view.notes_filter = filter;
    }
}

fn parse_tags(value: &str) -> Vec<String> {
    let mut tags = Vec::new();
    for candidate in value
        .split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
    {
        if !tags
            .iter()
            .any(|tag: &String| tag.eq_ignore_ascii_case(candidate))
        {
            tags.push(candidate.to_owned());
        }
        if tags.len() == lumi_core::MAX_ANNOTATION_TAGS {
            break;
        }
    }
    tags
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn highlight_style_label(style: HighlightStyle) -> &'static str {
    match style {
        HighlightStyle::Yellow => "жёлтое",
        HighlightStyle::Bold => "жирное",
        HighlightStyle::Green => "зелёное",
        HighlightStyle::Blue => "синее",
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
    style: HighlightStyle,
    csrf: Signal<String>,
    save_state: Signal<SaveState>,
) {
    create_annotation_optimistic(
        state,
        anchor,
        AnnotationTarget::TextRange,
        AnnotationKind::Highlight { style },
        None,
        Vec::new(),
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
    let (body, target, title, tags) = match &*state.read() {
        ReaderState::Ready(view) => (
            view.note_draft.trim().to_owned(),
            view.draft_target.clone(),
            non_empty(view.note_title_draft.trim()),
            parse_tags(&view.note_tags_draft),
        ),
        _ => return,
    };
    if !body.is_empty() {
        create_annotation_optimistic(
            state,
            anchor,
            target,
            AnnotationKind::Note { body },
            title,
            tags,
            csrf,
            save_state,
        );
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "optimistic Reader mutation mirrors Annotation v2 metadata and UI signals"
)]
fn create_annotation_optimistic(
    mut state: Signal<ReaderState>,
    anchor: Anchor,
    target: AnnotationTarget,
    kind: AnnotationKind,
    title: Option<String>,
    tags: Vec<String>,
    csrf: Signal<String>,
    save_state: Signal<SaveState>,
) {
    let (material_id, revision_id) = match &*state.read() {
        ReaderState::Ready(view) => (view.entry.id, view.document.revision_id),
        _ => return,
    };
    let temporary_id = Uuid::now_v7();
    let annotation_type = kind.annotation_type(&target);
    let temporary = Annotation {
        id: temporary_id,
        material_id,
        revision_id,
        anchor: anchor.clone(),
        annotation_type,
        target: target.clone(),
        kind: kind.clone(),
        title: title.clone(),
        tags: tags.clone(),
        status: AnnotationStatus::Active,
        related_annotation_id: None,
        revision: 0,
        created_at: 0,
        updated_at: 0,
    };
    let pending = PendingMutation::Create {
        command: Box::new(CreateAnnotationCommand {
            material_id,
            revision_id,
            anchor,
            target,
            kind,
            title,
            tags,
            status: AnnotationStatus::Active,
            related_annotation_id: None,
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
        view.voice_composer_open = false;
        view.voice_recording = false;
        view.voice_recorded = None;
        if !view.voice_preview_url.is_empty() {
            crate::voice::revoke_preview(&view.voice_preview_url);
        }
        view.voice_preview_url.clear();
        view.voice_uploading = false;
        view.voice_error = None;
        view.link_suggestions.clear();
        view.note_title_draft.clear();
        view.note_draft.clear();
        view.note_tags_draft.clear();
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
    let (draft, title, tags) = match &*state.read() {
        ReaderState::Ready(view) => (
            view.edit_note_draft.trim().to_owned(),
            non_empty(view.edit_note_title.trim()),
            parse_tags(&view.edit_note_tags),
        ),
        _ => return,
    };
    if draft.is_empty() {
        return;
    }
    if let ReaderState::Ready(view) = &mut *state.write() {
        view.editing_note = None;
    }
    update_annotation_optimistic(
        state,
        previous,
        AnnotationKind::Note { body: draft },
        title,
        tags,
        None,
        csrf,
        save_state,
    );
}

fn update_highlight_style(
    state: Signal<ReaderState>,
    previous: Annotation,
    style: HighlightStyle,
    csrf: Signal<String>,
    save_state: Signal<SaveState>,
) {
    update_annotation_optimistic(
        state,
        previous,
        AnnotationKind::Highlight { style },
        None,
        Vec::new(),
        None,
        csrf,
        save_state,
    );
}

fn toggle_annotation_status(
    state: Signal<ReaderState>,
    previous: Annotation,
    csrf: Signal<String>,
    save_state: Signal<SaveState>,
) {
    let status = if previous.status == AnnotationStatus::Active {
        AnnotationStatus::Archived
    } else {
        AnnotationStatus::Active
    };
    update_annotation_optimistic(
        state,
        previous.clone(),
        previous.kind,
        previous.title,
        previous.tags,
        Some(status),
        csrf,
        save_state,
    );
}

#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the public Annotation v2 command"
)]
fn update_annotation_optimistic(
    mut state: Signal<ReaderState>,
    previous: Annotation,
    kind: AnnotationKind,
    title: Option<String>,
    tags: Vec<String>,
    status: Option<AnnotationStatus>,
    csrf: Signal<String>,
    save_state: Signal<SaveState>,
) {
    let status = status.unwrap_or(previous.status);
    let command = UpdateAnnotationCommand {
        material_id: previous.material_id,
        annotation_id: previous.id,
        expected_revision: previous.revision,
        target: previous.target.clone(),
        kind: kind.clone(),
        title: title.clone().or(previous.title.clone()),
        tags: if tags.is_empty() {
            previous.tags.clone()
        } else {
            tags
        },
        status,
        related_annotation_id: previous.related_annotation_id,
    };
    let pending = PendingMutation::Update {
        command: command.clone(),
        idempotency_key: Uuid::now_v7().to_string(),
    };
    if let ReaderState::Ready(view) = &mut *state.write() {
        if let Some(item) = view
            .annotations
            .iter_mut()
            .find(|item| item.annotation.id == previous.id)
        {
            item.annotation.annotation_type = kind.annotation_type(&item.annotation.target);
            item.annotation.kind = kind;
            item.annotation.title = command.title.clone();
            item.annotation.tags = command.tags.clone();
            item.annotation.status = command.status;
            item.sync_state = ItemSyncState::Saving;
            item.pending = Some(pending.clone());
        }
    }
    begin_save(save_state, pending_key(&pending), pending_subject(&pending));
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
    let material_id = match &pending {
        PendingMutation::Create { command, .. } => command.material_id,
        PendingMutation::Update { command, .. } => command.material_id,
        PendingMutation::Delete { command, .. } => command.material_id,
    };
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
                        view.annotation_links.remove(&local_id);
                    }
                } else {
                    replace_annotation(&mut state, local_id, annotation);
                }
                if let Ok(links) = get_json::<Vec<AnnotationLink>>(&format!(
                    "/materials/{material_id}/annotation-links"
                ))
                .await
                {
                    if let ReaderState::Ready(view) = &mut *state.write() {
                        view.annotation_links.clear();
                        for link in links {
                            view.annotation_links
                                .entry(link.source_annotation_id)
                                .or_default()
                                .push(link);
                        }
                    }
                }
                let annotation_ids = match &*state.read() {
                    ReaderState::Ready(view) => view
                        .annotations
                        .iter()
                        .map(|item| item.annotation.id)
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                let mut backlinks = HashMap::new();
                for annotation_id in annotation_ids {
                    if let Ok(items) = get_json::<Vec<AnnotationBacklink>>(&format!(
                        "/links/backlinks/annotation/{annotation_id}"
                    ))
                    .await
                    {
                        if !items.is_empty() {
                            backlinks.insert(annotation_id, items);
                        }
                    }
                }
                if let ReaderState::Ready(view) = &mut *state.write() {
                    view.annotation_backlinks = backlinks;
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
                    Some("Не удалось найти это место в текущей версии материала.".to_owned());
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
                Some("Не удалось найти это место в текущей версии материала.".to_owned());
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

fn set_document_title(title: &str) {
    if let Some(document) = web_sys::window().and_then(|window| window.document()) {
        document.set_title(title);
    }
}

fn reset_reader_page_view() {
    spawn_forever(async move {
        browser_delay(20).await;
        if let Some(window) = web_sys::window() {
            window.scroll_to_with_x_and_y(0.0, 0.0);
        }
        focus_reader_node("reader-page-surface");
    });
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
    close_native_reader_dialog("reader-footnote-dialog");
    let target = if let ReaderState::Ready(view) = &mut *state.write() {
        if view.footnote.take().is_some() {
            None
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
            crate::voice::cancel_recording();
            if !view.voice_preview_url.is_empty() {
                crate::voice::revoke_preview(&view.voice_preview_url);
            }
            view.selected_anchor = None;
            view.note_composer_open = false;
            view.voice_composer_open = false;
            view.voice_recording = false;
            view.voice_recorded = None;
            view.voice_preview_url.clear();
            view.voice_uploading = false;
            view.voice_error = None;
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
    crate::voice::cancel_recording();
    if let ReaderState::Ready(view) = &mut *state.write() {
        if !view.voice_preview_url.is_empty() {
            crate::voice::revoke_preview(&view.voice_preview_url);
        }
        view.selected_anchor = None;
        view.note_composer_open = false;
        view.voice_composer_open = false;
        view.voice_recording = false;
        view.voice_recorded = None;
        view.voice_preview_url.clear();
        view.voice_uploading = false;
        view.voice_error = None;
        view.link_suggestions.clear();
        view.note_title_draft.clear();
        view.note_draft.clear();
        view.note_tags_draft.clear();
        view.draft_target = AnnotationTarget::TextRange;
    }
    defer_reader_focus("reader-page-surface");
}

fn close_footnote(mut state: Signal<ReaderState>) {
    close_native_reader_dialog("reader-footnote-dialog");
    if let ReaderState::Ready(view) = &mut *state.write() {
        view.footnote = None;
    }
}

fn close_native_reader_dialog(id: &str) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Some(element) = document.get_element_by_id(id) else {
        return;
    };
    let Ok(dialog) = element.dyn_into::<web_sys::HtmlDialogElement>() else {
        return;
    };
    if dialog.open() {
        dialog.close();
    }
}

fn defer_reader_focus(id: &str) {
    let id = id.to_owned();
    spawn_forever(async move {
        let mut focused_last_tick = false;
        for _ in 0..10 {
            browser_delay(20).await;
            let focused = reader_node_is_focused(&id);
            if focused && focused_last_tick {
                break;
            }
            if !focused {
                focus_reader_node(&id);
            }
            focused_last_tick = reader_node_is_focused(&id);
        }
    });
}

fn reader_node_is_focused(id: &str) -> bool {
    web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.active_element())
        .is_some_and(|element| element.id() == id)
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
        dismiss_annotation_message_later(state, "Экспорт подготовлен");
    });
}

fn dismiss_annotation_message_later(mut state: Signal<ReaderState>, expected: &'static str) {
    spawn(async move {
        browser_delay(3_000).await;
        if let ReaderState::Ready(view) = &mut *state.write() {
            if view.annotation_message.as_deref() == Some(expected) {
                view.annotation_message = None;
            }
        }
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

async fn post_reader_json<T, R>(path: &str, body: &T, csrf: &str) -> Result<R, String>
where
    T: serde::Serialize,
    R: for<'de> serde::Deserialize<'de>,
{
    let request = Request::post(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .json(body)
        .map_err(|error| error.to_string())?;
    let response = request.send().await.map_err(|error| error.to_string())?;
    if response.status() == 401 {
        super::account::notify_session_expired();
    }
    if !response.ok() {
        return Err(format!(
            "Lumi API отклонил запрос (HTTP {}). Проверьте формат, размер и подключение.",
            response.status()
        ));
    }
    response.json().await.map_err(|error| error.to_string())
}

async fn post_reader_empty<R>(path: &str, csrf: &str) -> Result<R, String>
where
    R: for<'de> serde::Deserialize<'de>,
{
    let response = Request::post(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status() == 401 {
        super::account::notify_session_expired();
    }
    if !response.ok() {
        return Err(format!("Lumi API вернул HTTP {}.", response.status()));
    }
    response.json().await.map_err(|error| error.to_string())
}

async fn put_audio_bytes(
    path: &str,
    media_type: &str,
    bytes: Vec<u8>,
    csrf: &str,
) -> Result<(), String> {
    let response = Request::put(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .header("Content-Type", media_type)
        .body(bytes)
        .map_err(|error| error.to_string())?
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status() == 401 {
        super::account::notify_session_expired();
    }
    if response.ok() {
        Ok(())
    } else {
        Err(format!(
            "Аудио не загружено: Lumi API вернул HTTP {}.",
            response.status()
        ))
    }
}

fn resolve_annotation_link(
    mut state: Signal<ReaderState>,
    link_id: Uuid,
    target: LinkTarget,
    csrf: Signal<String>,
) {
    let csrf_token = csrf.read().clone();
    spawn(async move {
        match post_reader_json::<_, AnnotationLink>(
            "/links/resolve",
            &ResolveAnnotationLinkCommand { link_id, target },
            &csrf_token,
        )
        .await
        {
            Ok(resolved) => {
                if let ReaderState::Ready(view) = &mut *state.write() {
                    let links = view
                        .annotation_links
                        .entry(resolved.source_annotation_id)
                        .or_default();
                    if let Some(current) = links.iter_mut().find(|link| link.id == resolved.id) {
                        *current = resolved;
                    }
                    view.annotation_message = Some("Ссылка уточнена.".to_owned());
                }
            }
            Err(error) => set_annotation_message(&mut state, error),
        }
    });
}

fn navigate_to_link_target(
    mut state: Signal<ReaderState>,
    target: LinkTarget,
    csrf: Signal<String>,
    generation: Signal<u64>,
    in_flight: Signal<bool>,
    save_state: Signal<SaveState>,
) {
    let current_material = match &*state.read() {
        ReaderState::Ready(view) => view.entry.id,
        _ => return,
    };
    if target.material_id == Some(current_material) {
        if let Some(anchor) = target.anchor {
            navigate_to_annotation(state, &anchor, csrf, generation, in_flight, save_state);
            return;
        }
        if target.object_type == LinkTargetType::Annotation {
            let anchor = match &*state.read() {
                ReaderState::Ready(view) => view
                    .annotations
                    .iter()
                    .find(|item| item.annotation.id == target.object_id)
                    .map(|item| item.annotation.anchor.clone()),
                _ => None,
            };
            if let Some(anchor) = anchor {
                navigate_to_annotation(state, &anchor, csrf, generation, in_flight, save_state);
                return;
            }
        }
        if let ReaderState::Ready(view) = &mut *state.write() {
            view.navigation.jump_to(0, view.page_map.pages.len());
            view.notes_open = false;
            persist_current(view, csrf, generation, in_flight, save_state);
        }
        return;
    }
    if let Some(material_id) = target.material_id {
        if let Some(window) = web_sys::window() {
            let _ = window.location().set_hash(&format!("reader/{material_id}"));
        }
    }
}

fn navigate_to_backlink(
    state: Signal<ReaderState>,
    backlink: AnnotationBacklink,
    csrf: Signal<String>,
    generation: Signal<u64>,
    in_flight: Signal<bool>,
    save_state: Signal<SaveState>,
) {
    let (current_material, anchor) = match &*state.read() {
        ReaderState::Ready(view) => (
            view.entry.id,
            view.annotations
                .iter()
                .find(|item| item.annotation.id == backlink.source_annotation_id)
                .map(|item| item.annotation.anchor.clone()),
        ),
        _ => return,
    };
    if backlink.source_material_id == current_material {
        if let Some(anchor) = anchor {
            navigate_to_annotation(state, &anchor, csrf, generation, in_flight, save_state);
        }
    } else if let Some(window) = web_sys::window() {
        let _ = window
            .location()
            .set_hash(&format!("reader/{}", backlink.source_material_id));
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
    let moved = if let ReaderState::Ready(current) = &mut *state.write() {
        let previous = current.navigation.current();
        current
            .navigation
            .move_to(page, current.page_map.pages.len());
        persist_current(current, csrf, generation, in_flight, save_state);
        current.navigation.current() != previous
    } else {
        false
    };
    if moved {
        reset_reader_page_view();
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
    let navigated = if let ReaderState::Ready(current) = &mut *state.write() {
        if let Some(page) = current.page_map.page_for_path(path) {
            current
                .navigation
                .jump_to(page, current.page_map.pages.len());
            current.toc_open = false;
            persist_current(current, csrf, generation, in_flight, save_state);
            true
        } else {
            false
        }
    } else {
        false
    };
    if navigated {
        reset_reader_page_view();
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
    let navigated = if let ReaderState::Ready(current) = &mut *state.write() {
        if link.kind == ReadingLinkKind::Footnote {
            current.footnote = Some(link);
            false
        } else if let Some(page) = current.page_map.page_for_path(&link.target_path) {
            current
                .navigation
                .jump_to(page, current.page_map.pages.len());
            persist_current(current, csrf, generation, in_flight, save_state);
            true
        } else {
            false
        }
    } else {
        false
    };
    if navigated {
        reset_reader_page_view();
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
        let previous_settings = current.settings;
        let current_boundary = current
            .page_map
            .pages
            .get(current.navigation.current())
            .map(|page| page.start.clone());
        update(&mut current.settings);
        current.settings = current.settings.normalized();
        let layout_changed = previous_settings.font_size_px != current.settings.font_size_px
            || previous_settings.line_height_percent != current.settings.line_height_percent
            || previous_settings.width != current.settings.width;
        if layout_changed {
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
    let Some(command) = current_progress_command(current) else {
        return;
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

fn current_progress_command(current: &ReaderView) -> Option<MoveReadingPositionCommand> {
    let page = current.page_map.pages.get(current.navigation.current())?;
    let block = current.plan.block(&page.start.node_path)?;
    let mut locator = block.anchor.clone();
    locator.text_range = Some(TextRange {
        start: page.start.offset,
        end: page.start.offset,
    });
    locator.quote.clear();
    let page_count = current.page_map.pages.len().max(1);
    Some(MoveReadingPositionCommand {
        material_id: current.entry.id,
        revision_id: current.document.revision_id,
        locator,
        progress_fraction: (current.navigation.current() + 1) as f32 / page_count as f32,
    })
}

fn page_unit_id(document: &ReadingDocument, page: &ReaderPage) -> Option<String> {
    let root_path = page.fragments.first()?.node_path.first()?;
    document
        .nodes
        .iter()
        .find(|node| node.path.first() == Some(root_path))
        .map(|node| node.id.clone())
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
        Vec<AnnotationLink>,
        HashMap<Uuid, Vec<AnnotationBacklink>>,
        HashMap<Uuid, TranscriptArtifact>,
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
    let annotations: Vec<Annotation> =
        get_json(&format!("/materials/{material_id}/annotations")).await?;
    let links = get_json(&format!("/materials/{material_id}/annotation-links"))
        .await
        .unwrap_or_default();
    let mut backlinks = HashMap::new();
    for annotation in &annotations {
        if let Ok(items) = get_json::<Vec<AnnotationBacklink>>(&format!(
            "/links/backlinks/annotation/{}",
            annotation.id
        ))
        .await
        {
            if !items.is_empty() {
                backlinks.insert(annotation.id, items);
            }
        }
    }
    let mut transcripts = HashMap::new();
    for attachment_id in annotations.iter().filter_map(|annotation| {
        if let AnnotationKind::VoiceNote {
            audio_attachment_id,
            transcript_artifact_id: Some(_),
            ..
        } = &annotation.kind
        {
            Some(*audio_attachment_id)
        } else {
            None
        }
    }) {
        if let Ok(transcript) = get_json::<TranscriptArtifact>(&format!(
            "/audio/attachments/{attachment_id}/transcript"
        ))
        .await
        {
            transcripts.insert(attachment_id, transcript);
        }
    }
    Ok((
        entry,
        document,
        settings,
        progress,
        annotations,
        links,
        backlinks,
        transcripts,
    ))
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

fn browser_page_dimensions(settings: ReaderSettings) -> Result<(f64, f64), String> {
    let window = web_sys::window().ok_or_else(|| "Browser window недоступен.".to_owned())?;
    let viewport_width = window
        .inner_width()
        .map_err(|_| "Не удалось измерить viewport.".to_owned())?
        .as_f64()
        .unwrap_or(1024.0);
    let viewport_height = window
        .inner_height()
        .map_err(|_| "Не удалось измерить высоту окна.".to_owned())?
        .as_f64()
        .unwrap_or(900.0);
    let width: f64 = match settings.width {
        ReaderWidth::Narrow => 560.0_f64,
        ReaderWidth::Balanced => 680.0_f64,
        ReaderWidth::Wide => 820.0_f64,
    }
    .min((viewport_width - 32.0).max(300.0));
    let reader_chrome_height = if viewport_width <= 760.0 {
        198.0
    } else {
        186.0
    };
    let height = (viewport_height - reader_chrome_height).clamp(320.0, 680.0);
    Ok((width, height))
}

fn browser_page_map(plan: &RenderPlan, settings: ReaderSettings) -> Result<Rc<PageMap>, String> {
    let window = web_sys::window().ok_or_else(|| "Browser window недоступен.".to_owned())?;
    let (width, height) = browser_page_dimensions(settings)?;
    let layout_key = format!(
        "{}:{:.0}x{:.0}:{}:browser-page-map-v2",
        plan.revision_id,
        width,
        height,
        settings.layout_cache_key()
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
    let page_map = Rc::new(measured?);
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
            if block.atomic && !fragments.is_empty() {
                finish_page(&mut pages, &mut fragments, page);
                continue;
            }
            let whole = measurement_block(document, block, text, start, end)?;
            page.append_child(&whole)
                .map_err(|_| "Не удалось измерить reader block.".to_owned())?;
            if page_fits(page) {
                fragments.push(PageFragment {
                    node_path: block.node_path.clone(),
                    range: TextRange { start, end },
                });
                start = end;
                if block.atomic {
                    finish_page(&mut pages, &mut fragments, page);
                }
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
                finish_page(&mut pages, &mut fragments, page);
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
            accepted = snap_page_break(text, start, accepted.max(start + 1).min(end), end);
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

fn snap_page_break(text: &str, start: usize, accepted: usize, end: usize) -> usize {
    if accepted >= end {
        return end;
    }
    let characters = text.chars().collect::<Vec<_>>();
    if accepted == 0
        || accepted >= characters.len()
        || !characters[accepted.saturating_sub(1)].is_alphanumeric()
        || !characters[accepted].is_alphanumeric()
    {
        return accepted;
    }
    (start + 1..accepted)
        .rev()
        .find(|index| {
            let previous = characters[index.saturating_sub(1)];
            let next = characters[*index];
            !previous.is_alphanumeric() || !next.is_alphanumeric()
        })
        .unwrap_or(accepted)
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
        let height = match block.kind {
            lumi_core::ReadingNodeKind::HorizontalRule => "1px",
            lumi_core::ReadingNodeKind::Table => "180px",
            lumi_core::ReadingNodeKind::Image => "min(360px, calc(100% - 1em))",
            _ => "160px",
        };
        element
            .set_attribute(
                "style",
                &format!("height: {height}; margin: 0; break-inside: avoid; overflow: hidden"),
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
