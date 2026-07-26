//! Browser adapter for fixed-layout PDF revisions rendered by PDF.js.

use dioxus::prelude::*;
use gloo_net::http::Request;
use lumi_core::{
    AiContextAttachment, AiSourceScope, Anchor, Annotation, AnnotationKind, AnnotationStatus,
    AnnotationTarget, CreateAnnotationCommand, DeleteAnnotationCommand, HighlightStyle,
    LibraryEntry, MaterialKind, MoveReadingPositionCommand, PageFidelityDocument, PageRect,
    PdfPage, PdfRect, PdfSourceLocator, ReadingProgress, SourceLocator, TextRange,
    UpdateAnnotationCommand,
};
use serde_json::json;
use uuid::Uuid;
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{CustomEvent, EventTarget, RequestCredentials};

use super::account::API_BASE;

const PDF_CONTAINER_ID: &str = "lumi-pdf-pages";

#[derive(Clone)]
enum ReaderRouteState {
    Resolving,
    Reflowable,
    Pdf,
    Failed(String),
}

#[derive(Clone)]
enum PdfReaderState {
    Loading,
    Ready(Box<PdfReaderData>),
    Failed(String),
}

#[derive(Clone, PartialEq)]
struct PdfReaderData {
    entry: LibraryEntry,
    document: PageFidelityDocument,
}

#[derive(Clone, Debug)]
struct PdfSelection {
    page_index: u32,
    quote: String,
    rects: Vec<PdfRect>,
}

/// Resolve the material format before selecting the reflowable or fixed-layout reader.
#[component]
pub(crate) fn ReaderRoute(
    material_id: Uuid,
    csrf_token: String,
    on_close: EventHandler<()>,
    on_open_learning_session: EventHandler<Uuid>,
    on_manage_learning: EventHandler<(Uuid, Uuid)>,
) -> Element {
    let mut state = use_signal(|| ReaderRouteState::Resolving);
    use_effect(move || {
        state.set(ReaderRouteState::Resolving);
        spawn(async move {
            match get_json::<LibraryEntry>(&format!("/materials/{material_id}")).await {
                Ok(entry) if entry.kind == MaterialKind::Pdf => state.set(ReaderRouteState::Pdf),
                Ok(_) => state.set(ReaderRouteState::Reflowable),
                Err(error) => state.set(ReaderRouteState::Failed(error)),
            }
        });
    });

    let snapshot = state.read().clone();
    match snapshot {
        ReaderRouteState::Resolving => loading_view("Определяем формат материала…"),
        ReaderRouteState::Reflowable => rsx! {
            crate::reader::ReaderApp {
                material_id,
                csrf_token,
                on_close,
                on_open_learning_session,
                on_manage_learning,
            }
        },
        ReaderRouteState::Pdf => rsx! {
            PdfReaderApp {
                material_id,
                csrf_token,
                on_close,
                on_open_learning_session,
                on_manage_learning,
            }
        },
        ReaderRouteState::Failed(error) => rsx! {
            main { id: "main-content", class: "reader-loading", aria_label: "Ошибка чтения",
                h1 { "Не удалось открыть материал" }
                p { class: "library-alert", role: "alert", "{error}" }
                button { class: "secondary-action", r#type: "button", onclick: move |_| on_close.call(()), "Вернуться в библиотеку" }
            }
        },
    }
}

#[component]
fn PdfReaderApp(
    material_id: Uuid,
    csrf_token: String,
    on_close: EventHandler<()>,
    on_open_learning_session: EventHandler<Uuid>,
    on_manage_learning: EventHandler<(Uuid, Uuid)>,
) -> Element {
    let mut state = use_signal(|| PdfReaderState::Loading);
    let mut annotations = use_signal(Vec::<Annotation>::new);
    let mut current_page = use_signal(|| 0_u32);
    let mut zoom = use_signal(|| 1.0_f64);
    let mut selected_anchor = use_signal(|| None::<Anchor>);
    let mut selected_target = use_signal(|| AnnotationTarget::PageArea {
        page_index: 0,
        exact: true,
    });
    let mut note_title = use_signal(String::new);
    let mut note_draft = use_signal(String::new);
    let mut note_tags = use_signal(String::new);
    let mut reader_message = use_signal(String::new);
    let save_message = use_signal(|| "Сохранено".to_owned());
    let mut mount_config = use_signal(|| None::<String>);
    let progress_generation = use_signal(|| 0_u64);
    let csrf = use_signal(|| csrf_token);

    use_effect(move || {
        let Some(window) = web_sys::window() else {
            return;
        };
        let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            apply_pdf_ai_reader_target(
                material_id,
                state,
                current_page,
                selected_anchor,
                selected_target,
                reader_message,
            );
        });
        let _ = window.add_event_listener_with_callback(
            crate::ai::READER_TARGET_EVENT,
            handler.as_ref().unchecked_ref(),
        );
        handler.forget();
    });

    use_effect(move || {
        state.set(PdfReaderState::Loading);
        spawn(async move {
            match load_pdf_reader(material_id).await {
                Ok((data, progress, loaded_annotations)) => {
                    let target = crate::ai::take_reader_target(material_id);
                    let target_anchor = target.as_ref().and_then(|attachment| {
                        if attachment.revision_id != data.document.revision_id {
                            return None;
                        }
                        match &attachment.scope {
                            AiSourceScope::Selection { anchor, .. } => {
                                Some(anchor.as_ref().clone())
                            }
                            _ => None,
                        }
                    });
                    let target_page = target_anchor.as_ref().and_then(|anchor| {
                        match anchor.source_locator.as_ref() {
                            Some(SourceLocator::Pdf(locator)) => Some(locator.page_index),
                            _ => None,
                        }
                    });
                    let initial_page = target_page
                        .unwrap_or_else(|| restored_pdf_page(&progress))
                        .min(data.document.pages.len().saturating_sub(1) as u32);
                    let config = json!({
                        "containerId": PDF_CONTAINER_ID,
                        "sourceUrl": format!("{API_BASE}/materials/{material_id}/source"),
                        "initialPage": initial_page,
                        "pages": data.document.pages,
                        "annotations": annotation_overlays(&loaded_annotations),
                    })
                    .to_string();
                    current_page.set(initial_page);
                    selected_anchor.set(target_anchor);
                    annotations.set(loaded_annotations);
                    state.set(PdfReaderState::Ready(Box::new(data)));
                    mount_config.set(Some(config));
                }
                Err(error) => state.set(PdfReaderState::Failed(error)),
            }
        });
    });

    use_effect(move || {
        let Some(config) = mount_config.read().clone() else {
            return;
        };
        spawn(async move {
            browser_delay(30).await;
            if let Err(error) = mount_pdf(
                &config,
                state,
                annotations,
                current_page,
                selected_anchor,
                selected_target,
                reader_message,
                save_message,
                progress_generation,
                csrf,
            )
            .await
            {
                reader_message.set(error);
            }
        });
    });

    let snapshot = state.read().clone();
    match snapshot {
        PdfReaderState::Loading => loading_view("Готовим страницы PDF…"),
        PdfReaderState::Failed(error) => rsx! {
            main { id: "main-content", class: "reader-loading", aria_label: "Ошибка чтения PDF",
                p { class: "eyebrow", "PDF недоступен" }
                h1 { "Не удалось открыть PDF" }
                p { class: "library-alert", role: "alert", "{error}" }
                button { class: "secondary-action", r#type: "button", onclick: move |_| on_close.call(()), "Вернуться в библиотеку" }
            }
        },
        PdfReaderState::Ready(data) => {
            let title = data.document.title.clone();
            let creator = if data.document.creators.is_empty() {
                "Автор не указан".to_owned()
            } else {
                data.document.creators.join(", ")
            };
            let page_count = data.document.pages.len();
            let page_label = data
                .document
                .pages
                .get(current_page() as usize)
                .map_or_else(|| "—".to_owned(), |page| page.page_label.clone());
            let selected = selected_anchor.read().clone();
            let annotation_items = annotations.read().clone();
            let margin_data = data.as_ref().clone();
            let progress_percent = ((current_page() as usize + 1) * 100)
                .checked_div(page_count)
                .unwrap_or_default();
            rsx! {
                main {
                    id: "main-content",
                    class: "reader-workspace pdf-reader-workspace",
                    aria_label: "Чтение PDF {title}",
                    style: "--reader-progress: {progress_percent}%;",
                    header { class: "reader-topbar pdf-reader-topbar",
                        button { class: "reader-back", r#type: "button", aria_label: "Вернуться в библиотеку", onclick: move |_| {
                            destroy_pdf();
                            on_close.call(());
                        }, "← Библиотека" }
                        div { class: "reader-title",
                            h1 { "{title}" }
                            span { "{creator}" }
                        }
                        span { class: "reader-save-state saved", aria_live: "polite", "{save_message}" }
                        div { class: "reader-tools", aria_label: "Управление PDF",
                            button { r#type: "button", aria_label: "Предыдущая страница", disabled: current_page() == 0, onclick: move |_| {
                                let page = current_page().saturating_sub(1);
                                current_page.set(page);
                                call_pdf_two("goToPage", JsValue::from_str(PDF_CONTAINER_ID), JsValue::from_f64(f64::from(page)));
                            }, "←" }
                            span { class: "pdf-page-indicator", "Стр. {page_label} · {current_page() + 1}/{page_count}" }
                            button { r#type: "button", aria_label: "Следующая страница", disabled: current_page() as usize + 1 >= page_count, onclick: move |_| {
                                let page = (current_page() + 1).min(page_count.saturating_sub(1) as u32);
                                current_page.set(page);
                                call_pdf_two("goToPage", JsValue::from_str(PDF_CONTAINER_ID), JsValue::from_f64(f64::from(page)));
                            }, "→" }
                            button { r#type: "button", aria_label: "Уменьшить масштаб", onclick: move |_| {
                                let next = (zoom() - 0.1).max(0.5);
                                zoom.set(next);
                                call_pdf_two("setZoom", JsValue::from_str(PDF_CONTAINER_ID), JsValue::from_f64(next));
                            }, "−" }
                            output { aria_label: "Масштаб", "{(zoom() * 100.0).round()}%" }
                            button { r#type: "button", aria_label: "Увеличить масштаб", onclick: move |_| {
                                let next = (zoom() + 0.1).min(3.0);
                                zoom.set(next);
                                call_pdf_two("setZoom", JsValue::from_str(PDF_CONTAINER_ID), JsValue::from_f64(next));
                            }, "+" }
                            button { r#type: "button", onclick: move |_| {
                                if let Some(anchor) = anchor_for_page(&margin_data, current_page()) {
                                    selected_anchor.set(Some(anchor));
                                    selected_target.set(AnnotationTarget::PageArea {
                                        page_index: current_page(),
                                        exact: false,
                                    });
                                    note_title.set(String::new());
                                    note_draft.set(String::new());
                                    note_tags.set(String::new());
                                }
                            }, "Запись на полях" }
                            crate::ai::SummaryAction {
                                material_id,
                                revision_id: data.document.revision_id,
                                scope_kind: lumi_core::SummaryScopeKind::Material,
                                scope_ref: "material".to_owned(),
                                label: "Саммари".to_owned(),
                                csrf_token: csrf.read().clone(),
                            }
                            a { class: "secondary-action", href: "{API_BASE}/materials/{material_id}/source", download: "{data.entry.source_identity.source_name}", "Скачать PDF" }
                        }
                        div { class: "reader-chapter-progress", aria_hidden: "true", span {} }
                    }
                    if !reader_message().is_empty() {
                        p { class: "reader-global-status", role: "alert", "{reader_message}" }
                    }
                    div { class: "pdf-reader-layout",
                        section { class: "pdf-reader-stage", aria_label: "Страницы PDF",
                            div {
                                id: PDF_CONTAINER_ID,
                                class: "pdf-pages",
                                tabindex: "0",
                                aria_label: "Прокручиваемые страницы PDF",
                            }
                        }
                        aside { class: "pdf-annotation-panel", aria_label: "Аннотации PDF",
                            h2 { "Аннотации" }
                            crate::ai::SummaryAction {
                                material_id,
                                revision_id: data.document.revision_id,
                                scope_kind: lumi_core::SummaryScopeKind::Chapter,
                                scope_ref: format!("page:{}", current_page()),
                                label: "Саммари страницы".to_owned(),
                                csrf_token: csrf.read().clone(),
                            }
                            if let Some(anchor) = selected {
                                div { class: "pdf-selection-card",
                                    p { class: "eyebrow", "Выбранный фрагмент" }
                                    blockquote { "{anchor.quote}" }
                                    PdfAiActions {
                                        data: data.as_ref().clone(),
                                        anchor: anchor.clone(),
                                        reader_message,
                                    }
                                    div { class: "dialog-actions",
                                        button { class: "primary-action", r#type: "button", disabled: !selected_target.read().is_exact_selection(), onclick: move |_| {
                                            create_pdf_annotation(
                                                AnnotationKind::Highlight { style: HighlightStyle::Yellow },
                                                None,
                                                Vec::new(),
                                                state,
                                                selected_anchor,
                                                selected_target,
                                                annotations,
                                                save_message,
                                                reader_message,
                                                csrf,
                                            );
                                        }, "Жёлтым" }
                                        button { class: "primary-action", r#type: "button", disabled: !selected_target.read().is_exact_selection(), onclick: move |_| {
                                            create_pdf_annotation(
                                                AnnotationKind::Highlight { style: HighlightStyle::Bold },
                                                None,
                                                Vec::new(),
                                                state,
                                                selected_anchor,
                                                selected_target,
                                                annotations,
                                                save_message,
                                                reader_message,
                                                csrf,
                                            );
                                        }, "Жирным" }
                                    }
                                    label { "Заголовок",
                                        input {
                                            name: "pdf_note_title",
                                            maxlength: "240",
                                            value: "{note_title}",
                                            placeholder: "Короткое название",
                                            oninput: move |event| note_title.set(event.value()),
                                        }
                                    }
                                    label { "Заметка",
                                        textarea {
                                            name: "pdf_note",
                                            rows: "3",
                                            value: "{note_draft}",
                                            placeholder: "Добавьте мысль…",
                                            oninput: move |event| note_draft.set(event.value()),
                                        }
                                    }
                                    label { "Теги",
                                        input {
                                            name: "pdf_note_tags",
                                            value: "{note_tags}",
                                            placeholder: "чтение, идея",
                                            oninput: move |event| note_tags.set(event.value()),
                                        }
                                    }
                                    button { class: "secondary-action", r#type: "button", disabled: note_draft().trim().is_empty(), onclick: move |_| {
                                        let body = note_draft().trim().to_owned();
                                        if !body.is_empty() {
                                            create_pdf_annotation(
                                                AnnotationKind::Note { body },
                                                non_empty(note_title().trim()),
                                                parse_tags(&note_tags()),
                                                state,
                                                selected_anchor,
                                                selected_target,
                                                annotations,
                                                save_message,
                                                reader_message,
                                                csrf,
                                            );
                                            note_title.set(String::new());
                                            note_draft.set(String::new());
                                            note_tags.set(String::new());
                                        }
                                    }, "Сохранить заметку" }
                                }
                            } else {
                                p { class: "capability-note", "Выделите текст на странице, чтобы создать подсветку или заметку." }
                            }
                            if annotation_items.is_empty() {
                                p { "Аннотаций пока нет." }
                            } else {
                                ol { class: "annotation-list",
                                    for annotation in annotation_items {
                                        PdfAnnotationItem {
                                            annotation,
                                            annotations,
                                            current_page,
                                            save_message,
                                            reader_message,
                                            csrf
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if current_page() as usize + 1 == page_count {
                        if let Some(locator) = anchor_for_page(&data, current_page()) {
                            crate::learning::CompletionOffer {
                                key: "{data.document.revision_id}:material",
                                progress: MoveReadingPositionCommand {
                                    material_id: data.entry.id,
                                    revision_id: data.document.revision_id,
                                    locator,
                                    progress_fraction: 1.0,
                                },
                                completion: lumi_core::CompleteReadingScopeCommand {
                                    material_id: data.entry.id,
                                    revision_id: data.document.revision_id,
                                    scope_kind: lumi_core::LearningScopeKind::Material,
                                    content_unit_id: None,
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
        }
    }
}

#[component]
fn PdfAiActions(data: PdfReaderData, anchor: Anchor, reader_message: Signal<String>) -> Element {
    let ask_anchor = anchor.clone();
    let explain_anchor = anchor.clone();
    let summary_anchor = anchor;
    let ask_data = data.clone();
    let explain_data = data.clone();
    rsx! {
        div { class: "dialog-actions ai-selection-actions",
            button { class: "primary-action", r#type: "button", onclick: move |_| {
                dispatch_pdf_ai_handoff(
                    &ask_data,
                    ask_anchor.clone(),
                    "",
                    false,
                    reader_message,
                );
            }, "Спросить ИИ" }
            button { class: "secondary-action", r#type: "button", onclick: move |_| {
                dispatch_pdf_ai_handoff(
                    &explain_data,
                    explain_anchor.clone(),
                    "Объясни выделенный фрагмент простыми словами, не теряя его смысл.",
                    true,
                    reader_message,
                );
            }, "Объясни проще" }
            button { class: "secondary-action", r#type: "button", onclick: move |_| {
                dispatch_pdf_ai_handoff(
                    &data,
                    summary_anchor.clone(),
                    "Кратко перескажи выделенный фрагмент и сохрани ключевые тезисы.",
                    true,
                    reader_message,
                );
            }, "Кратко перескажи" }
        }
    }
}

fn dispatch_pdf_ai_handoff(
    data: &PdfReaderData,
    anchor: Anchor,
    instruction: &str,
    auto_submit: bool,
    mut reader_message: Signal<String>,
) {
    let quote = anchor.quote.chars().take(80).collect::<String>();
    let handoff = crate::ai::ReaderAiHandoff {
        attachment: AiContextAttachment {
            kind: "selection".to_owned(),
            material_id: data.entry.id,
            revision_id: data.document.revision_id,
            scope: AiSourceScope::Selection {
                material_id: data.entry.id,
                revision_id: data.document.revision_id,
                anchor: Box::new(anchor),
            },
            display_label: format!("{} — «{quote}»", data.entry.display_title()),
        },
        instruction: instruction.to_owned(),
        auto_submit,
    };
    match crate::ai::dispatch_reader_handoff(&handoff) {
        Ok(()) => reader_message.set("Фрагмент прикреплён к AI-чату.".to_owned()),
        Err(message) => reader_message.set(message),
    }
}

fn loading_view(message: &str) -> Element {
    rsx! {
        main { id: "main-content", class: "reader-loading", aria_label: "Загрузка материала", aria_live: "polite",
            span { class: "loading-mark", aria_hidden: "true" }
            h1 { "{message}" }
        }
    }
}

async fn load_pdf_reader(
    material_id: Uuid,
) -> Result<(PdfReaderData, Option<ReadingProgress>, Vec<Annotation>), String> {
    let entry: LibraryEntry = get_json(&format!("/materials/{material_id}")).await?;
    let revision_id = entry
        .active_revision_id
        .ok_or_else(|| "У материала ещё нет готовой версии для чтения.".to_owned())?;
    let document = get_json(&format!("/revisions/{revision_id}/page-fidelity-document")).await?;
    let progress = get_json(&format!("/materials/{material_id}/progress")).await?;
    let annotations = get_json(&format!("/materials/{material_id}/annotations")).await?;
    Ok((PdfReaderData { entry, document }, progress, annotations))
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
        .map_err(|error| format!("Некорректный ответ PDF reader API: {error}"))
}

fn apply_pdf_ai_reader_target(
    material_id: Uuid,
    state: Signal<PdfReaderState>,
    mut current_page: Signal<u32>,
    mut selected_anchor: Signal<Option<Anchor>>,
    mut selected_target: Signal<AnnotationTarget>,
    mut reader_message: Signal<String>,
) {
    let Some(target) = crate::ai::take_reader_target(material_id) else {
        return;
    };
    let PdfReaderState::Ready(data) = &*state.read() else {
        return;
    };
    if target.revision_id != data.document.revision_id {
        reader_message.set("Источник ответа относится к другой версии материала.".to_owned());
        return;
    }
    let (page, anchor) = match target.scope {
        AiSourceScope::Selection { anchor, .. } => {
            let page = match anchor.source_locator.as_ref() {
                Some(SourceLocator::Pdf(locator)) => locator.page_index,
                _ => {
                    reader_message.set("Citation не содержит PDF page anchor.".to_owned());
                    return;
                }
            };
            (page, Some(*anchor))
        }
        AiSourceScope::Chapter { scope_ref, .. } => (
            scope_ref
                .strip_prefix("page:")
                .unwrap_or(&scope_ref)
                .parse()
                .unwrap_or_default(),
            None,
        ),
        AiSourceScope::Material { .. } => (0, None),
    };
    current_page.set(page);
    if anchor.is_some() {
        selected_target.set(AnnotationTarget::PageArea {
            page_index: page,
            exact: true,
        });
    }
    selected_anchor.set(anchor);
    call_pdf_two(
        "goToPage",
        JsValue::from_str(PDF_CONTAINER_ID),
        JsValue::from_f64(f64::from(page)),
    );
    reader_message.set("Открыт источник ответа AI.".to_owned());
}

fn restored_pdf_page(progress: &Option<ReadingProgress>) -> u32 {
    progress
        .as_ref()
        .and_then(|progress| progress.locator.source_locator.as_ref())
        .and_then(|locator| match locator {
            SourceLocator::Pdf(locator) => Some(locator.page_index),
            _ => None,
        })
        .unwrap_or_default()
}

fn anchor_for_page(data: &PdfReaderData, page_index: u32) -> Option<Anchor> {
    let page = data.document.pages.get(page_index as usize)?;
    Some(pdf_anchor(data, page, String::new(), Vec::new()))
}

fn anchor_for_selection(data: &PdfReaderData, selection: PdfSelection) -> Option<Anchor> {
    let page = data.document.pages.get(selection.page_index as usize)?;
    Some(pdf_anchor(data, page, selection.quote, selection.rects))
}

fn pdf_anchor(data: &PdfReaderData, page: &PdfPage, quote: String, rects: Vec<PdfRect>) -> Anchor {
    let path = vec![format!("page-{}", page.page_index)];
    let normalized_rects = rects
        .iter()
        .map(|rect| PdfRect {
            x: rect.x / page.width_points.max(1.0),
            y: rect.y / page.height_points.max(1.0),
            width: rect.width / page.width_points.max(1.0),
            height: rect.height / page.height_points.max(1.0),
        })
        .collect();
    let locator = SourceLocator::Pdf(PdfSourceLocator {
        pdf_file_checksum: data.entry.source_identity.source_hash.clone(),
        page_index: page.page_index,
        page_label: page.page_label.clone(),
        page_revision_hash: page.page_hash.clone(),
        page_rects: rects.clone(),
        page_quads: Vec::new(),
        text_layer_revision: Some("pdfjs-selection-v1".to_owned()),
        text_block_start: None,
        text_block_end: None,
        text_char_start: None,
        text_char_end: None,
        normalized_rects,
    });
    Anchor {
        revision_id: data.document.revision_id,
        node_path: path.clone(),
        end_node_path: path,
        text_range: (!quote.is_empty()).then(|| TextRange {
            start: 0,
            end: quote.chars().count(),
        }),
        quote,
        prefix: String::new(),
        suffix: String::new(),
        content_hash: page.page_hash.clone(),
        source_locator: Some(locator.clone()),
        end_source_locator: Some(locator),
        page_rects: rects
            .iter()
            .map(|rect| PageRect {
                page_index: page.page_index,
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            })
            .collect(),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "browser event bridge carries the independent reader signals it updates"
)]
async fn mount_pdf(
    config: &str,
    state: Signal<PdfReaderState>,
    annotations: Signal<Vec<Annotation>>,
    mut current_page: Signal<u32>,
    mut selected_anchor: Signal<Option<Anchor>>,
    mut selected_target: Signal<AnnotationTarget>,
    mut reader_message: Signal<String>,
    save_message: Signal<String>,
    progress_generation: Signal<u64>,
    csrf: Signal<String>,
) -> Result<(), String> {
    wait_for_pdf_api().await?;
    let container = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(PDF_CONTAINER_ID))
        .ok_or_else(|| "Контейнер PDF reader недоступен.".to_owned())?;
    let target: EventTarget = container
        .dyn_into()
        .map_err(|_| "Контейнер PDF reader не поддерживает события.".to_owned())?;

    let page_callback = Callback::new(move |event: CustomEvent| {
        let Some(page_index) = event_detail_u32(&event, "pageIndex") else {
            return;
        };
        current_page.set(page_index);
        schedule_progress_save(
            page_index,
            state,
            progress_generation,
            save_message,
            reader_message,
            csrf,
        );
    });
    let page_listener =
        Closure::<dyn FnMut(CustomEvent)>::new(move |event| page_callback.call(event));
    target
        .add_event_listener_with_callback("lumi-pdf-page", page_listener.as_ref().unchecked_ref())
        .map_err(js_error)?;
    page_listener.forget();

    let selection_callback = Callback::new(move |event: CustomEvent| {
        match selection_from_event(&event).and_then(|selection| match &*state.read() {
            PdfReaderState::Ready(data) => anchor_for_selection(data, selection),
            PdfReaderState::Loading | PdfReaderState::Failed(_) => None,
        }) {
            Some(anchor) => {
                let page_index = match anchor.source_locator.as_ref() {
                    Some(SourceLocator::Pdf(locator)) => locator.page_index,
                    _ => 0,
                };
                selected_target.set(AnnotationTarget::PageArea {
                    page_index,
                    exact: true,
                });
                selected_anchor.set(Some(anchor));
                reader_message.set(String::new());
            }
            None => reader_message.set("Не удалось привязать выделение к странице.".to_owned()),
        }
    });
    let selection_listener =
        Closure::<dyn FnMut(CustomEvent)>::new(move |event| selection_callback.call(event));
    target
        .add_event_listener_with_callback(
            "lumi-pdf-selection",
            selection_listener.as_ref().unchecked_ref(),
        )
        .map_err(js_error)?;
    selection_listener.forget();

    let error_callback = Callback::new(move |event: CustomEvent| {
        let message = event_detail_string(&event, "message")
            .unwrap_or_else(|| "PDF.js не смог отрисовать страницу.".to_owned());
        reader_message.set(message);
    });
    let error_listener =
        Closure::<dyn FnMut(CustomEvent)>::new(move |event| error_callback.call(event));
    target
        .add_event_listener_with_callback("lumi-pdf-error", error_listener.as_ref().unchecked_ref())
        .map_err(js_error)?;
    error_listener.forget();

    let result = call_pdf_one("mountJson", JsValue::from_str(config))?;
    if result.is_instance_of::<js_sys::Promise>() {
        JsFuture::from(js_sys::Promise::from(result))
            .await
            .map_err(js_error)?;
    }
    let overlays = serde_json::to_string(&annotation_overlays(&annotations.read()))
        .map_err(|error| error.to_string())?;
    call_pdf_two(
        "setAnnotationsJson",
        JsValue::from_str(PDF_CONTAINER_ID),
        JsValue::from_str(&overlays),
    );
    Ok(())
}

fn selection_from_event(event: &CustomEvent) -> Option<PdfSelection> {
    let page_index = event_detail_u32(event, "pageIndex")?;
    let quote = event_detail_string(event, "quote")?;
    let rects_value = js_sys::Reflect::get(&event.detail(), &JsValue::from_str("rects")).ok()?;
    let rects = js_sys::Array::from(&rects_value)
        .iter()
        .filter_map(|value| {
            Some(PdfRect {
                x: reflect_number(&value, "x")? as f32,
                y: reflect_number(&value, "y")? as f32,
                width: reflect_number(&value, "width")? as f32,
                height: reflect_number(&value, "height")? as f32,
            })
        })
        .filter(|rect| {
            rect.x.is_finite()
                && rect.y.is_finite()
                && rect.width.is_finite()
                && rect.height.is_finite()
                && rect.width > 0.0
                && rect.height > 0.0
        })
        .collect::<Vec<_>>();
    (!rects.is_empty()).then_some(PdfSelection {
        page_index,
        quote,
        rects,
    })
}

fn event_detail_u32(event: &CustomEvent, key: &str) -> Option<u32> {
    reflect_number(&event.detail(), key).and_then(|value| {
        (value.is_finite() && value >= 0.0 && value <= f64::from(u32::MAX)).then_some(value as u32)
    })
}

fn event_detail_string(event: &CustomEvent, key: &str) -> Option<String> {
    js_sys::Reflect::get(&event.detail(), &JsValue::from_str(key))
        .ok()?
        .as_string()
}

fn reflect_number(value: &JsValue, key: &str) -> Option<f64> {
    js_sys::Reflect::get(value, &JsValue::from_str(key))
        .ok()?
        .as_f64()
}

fn schedule_progress_save(
    page_index: u32,
    state: Signal<PdfReaderState>,
    mut generation: Signal<u64>,
    mut save_message: Signal<String>,
    mut reader_message: Signal<String>,
    csrf: Signal<String>,
) {
    let next_generation = generation().saturating_add(1);
    generation.set(next_generation);
    spawn(async move {
        browser_delay(450).await;
        if generation() != next_generation {
            return;
        }
        let command = match &*state.read() {
            PdfReaderState::Ready(data) => {
                let Some(locator) = anchor_for_page(data, page_index) else {
                    return;
                };
                MoveReadingPositionCommand {
                    material_id: data.entry.id,
                    revision_id: data.document.revision_id,
                    locator,
                    progress_fraction: (page_index + 1) as f32
                        / data.document.pages.len().max(1) as f32,
                }
            }
            PdfReaderState::Loading | PdfReaderState::Failed(_) => return,
        };
        save_message.set("Сохраняем позицию…".to_owned());
        match save_progress(&command, &csrf.read()).await {
            Ok(()) => save_message.set("Сохранено".to_owned()),
            Err(error) => {
                save_message.set("Позиция не сохранена".to_owned());
                reader_message.set(error);
            }
        }
    });
}

#[expect(
    clippy::too_many_arguments,
    reason = "PDF mutation bridges independent Dioxus signals and Annotation v2 metadata"
)]
fn create_pdf_annotation(
    kind: AnnotationKind,
    title: Option<String>,
    tags: Vec<String>,
    state: Signal<PdfReaderState>,
    mut selected_anchor: Signal<Option<Anchor>>,
    selected_target: Signal<AnnotationTarget>,
    mut annotations: Signal<Vec<Annotation>>,
    mut save_message: Signal<String>,
    mut reader_message: Signal<String>,
    csrf: Signal<String>,
) {
    let Some(anchor) = selected_anchor.read().clone() else {
        return;
    };
    let (material_id, revision_id) = match &*state.read() {
        PdfReaderState::Ready(data) => (data.entry.id, data.document.revision_id),
        PdfReaderState::Loading | PdfReaderState::Failed(_) => return,
    };
    let command = CreateAnnotationCommand {
        material_id,
        revision_id,
        anchor,
        target: selected_target.read().clone(),
        kind,
        title,
        tags,
        status: AnnotationStatus::Active,
        related_annotation_id: None,
    };
    save_message.set("Сохраняем аннотацию…".to_owned());
    spawn(async move {
        match post_annotation(&command, &csrf.read()).await {
            Ok(annotation) => {
                annotations.write().push(annotation);
                selected_anchor.set(None);
                save_message.set("Сохранено".to_owned());
                reader_message.set(String::new());
                update_annotation_overlays(&annotations.read());
                clear_browser_selection();
            }
            Err(error) => {
                save_message.set("Аннотация не сохранена".to_owned());
                reader_message.set(error);
            }
        }
    });
}

#[component]
fn PdfAnnotationItem(
    annotation: Annotation,
    annotations: Signal<Vec<Annotation>>,
    mut current_page: Signal<u32>,
    save_message: Signal<String>,
    reader_message: Signal<String>,
    csrf: Signal<String>,
) -> Element {
    let navigate_value = annotation.clone();
    let yellow_value = annotation.clone();
    let bold_value = annotation.clone();
    let delete_value = annotation.clone();
    let style = match annotation.kind {
        AnnotationKind::Highlight { style } => Some(style),
        AnnotationKind::Note { .. } | AnnotationKind::VoiceNote { .. } => None,
    };
    rsx! {
        li {
            button { r#type: "button", onclick: move |_| {
                if let Some(page) = pdf_annotation_page(&navigate_value) {
                    current_page.set(page);
                    call_pdf_two("goToPage", JsValue::from_str(PDF_CONTAINER_ID), JsValue::from_f64(f64::from(page)));
                }
            },
                strong { "{annotation_kind_label(&annotation.kind)}" }
                span { "{annotation.anchor.quote}" }
            }
            if let Some(title) = annotation.title.clone() { strong { "{title}" } }
            if !annotation.tags.is_empty() { span { "{annotation.tags.join(\", \")}" } }
            if let Some(style) = style {
                div { class: "annotation-style-actions",
                    button { r#type: "button", disabled: style == HighlightStyle::Yellow, onclick: move |_| update_pdf_highlight(yellow_value.clone(), HighlightStyle::Yellow, annotations, save_message, reader_message, csrf), "Жёлтый" }
                    button { r#type: "button", disabled: style == HighlightStyle::Bold, onclick: move |_| update_pdf_highlight(bold_value.clone(), HighlightStyle::Bold, annotations, save_message, reader_message, csrf), "Жирный" }
                }
            }
            button { class: "danger-link", r#type: "button", onclick: move |_| delete_pdf_annotation(delete_value.clone(), annotations, save_message, reader_message, csrf), "Удалить" }
        }
    }
}

fn update_pdf_highlight(
    previous: Annotation,
    style: HighlightStyle,
    mut annotations: Signal<Vec<Annotation>>,
    mut save_message: Signal<String>,
    mut reader_message: Signal<String>,
    csrf: Signal<String>,
) {
    let command = UpdateAnnotationCommand {
        material_id: previous.material_id,
        annotation_id: previous.id,
        expected_revision: previous.revision,
        target: previous.target,
        kind: AnnotationKind::Highlight { style },
        title: previous.title,
        tags: previous.tags,
        status: previous.status,
        related_annotation_id: previous.related_annotation_id,
    };
    save_message.set("Сохраняем стиль…".to_owned());
    spawn(async move {
        match put_annotation(&command, &csrf.read()).await {
            Ok(updated) => {
                if let Some(annotation) = annotations
                    .write()
                    .iter_mut()
                    .find(|annotation| annotation.id == updated.id)
                {
                    *annotation = updated;
                }
                save_message.set("Сохранено".to_owned());
                reader_message.set(String::new());
                update_annotation_overlays(&annotations.read());
            }
            Err(error) => {
                save_message.set("Стиль не сохранён".to_owned());
                reader_message.set(error);
            }
        }
    });
}

fn delete_pdf_annotation(
    annotation: Annotation,
    mut annotations: Signal<Vec<Annotation>>,
    mut save_message: Signal<String>,
    mut reader_message: Signal<String>,
    csrf: Signal<String>,
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
    save_message.set("Удаляем аннотацию…".to_owned());
    spawn(async move {
        match delete_annotation(&command, &csrf.read()).await {
            Ok(()) => {
                annotations
                    .write()
                    .retain(|stored| stored.id != command.annotation_id);
                save_message.set("Сохранено".to_owned());
                reader_message.set(String::new());
                update_annotation_overlays(&annotations.read());
            }
            Err(error) => {
                save_message.set("Аннотация не удалена".to_owned());
                reader_message.set(error);
            }
        }
    });
}

async fn post_annotation(
    command: &CreateAnnotationCommand,
    csrf: &str,
) -> Result<Annotation, String> {
    let request = Request::post(&format!(
        "{API_BASE}/materials/{}/annotations",
        command.material_id
    ))
    .credentials(RequestCredentials::Include)
    .header("X-Lumi-CSRF", csrf)
    .header("Idempotency-Key", &Uuid::now_v7().to_string())
    .json(command)
    .map_err(|error| error.to_string())?;
    let response = request.send().await.map_err(|error| error.to_string())?;
    if response.status() == 401 {
        super::account::notify_session_expired();
    }
    if !response.ok() {
        return Err(format!(
            "Аннотация не сохранена: HTTP {}.",
            response.status()
        ));
    }
    response
        .json()
        .await
        .map_err(|error| format!("Некорректный ответ API: {error}"))
}

async fn put_annotation(
    command: &UpdateAnnotationCommand,
    csrf: &str,
) -> Result<Annotation, String> {
    let request = Request::put(&format!(
        "{API_BASE}/materials/{}/annotations/{}",
        command.material_id, command.annotation_id
    ))
    .credentials(RequestCredentials::Include)
    .header("X-Lumi-CSRF", csrf)
    .header("Idempotency-Key", &Uuid::now_v7().to_string())
    .json(command)
    .map_err(|error| error.to_string())?;
    let response = request.send().await.map_err(|error| error.to_string())?;
    if response.status() == 401 {
        super::account::notify_session_expired();
    }
    if !response.ok() {
        return Err(format!("Стиль не сохранён: HTTP {}.", response.status()));
    }
    response
        .json()
        .await
        .map_err(|error| format!("Некорректный ответ API: {error}"))
}

async fn delete_annotation(command: &DeleteAnnotationCommand, csrf: &str) -> Result<(), String> {
    let request = Request::delete(&format!(
        "{API_BASE}/materials/{}/annotations/{}",
        command.material_id, command.annotation_id
    ))
    .credentials(RequestCredentials::Include)
    .header("X-Lumi-CSRF", csrf)
    .header("Idempotency-Key", &Uuid::now_v7().to_string())
    .json(command)
    .map_err(|error| error.to_string())?;
    let response = request.send().await.map_err(|error| error.to_string())?;
    if response.status() == 401 {
        super::account::notify_session_expired();
    }
    response
        .ok()
        .then_some(())
        .ok_or_else(|| format!("Аннотация не удалена: HTTP {}.", response.status()))
}

async fn save_progress(command: &MoveReadingPositionCommand, csrf: &str) -> Result<(), String> {
    let request = Request::put(&format!(
        "{API_BASE}/materials/{}/progress",
        command.material_id
    ))
    .credentials(RequestCredentials::Include)
    .header("X-Lumi-CSRF", csrf)
    .header("Idempotency-Key", &Uuid::now_v7().to_string())
    .json(command)
    .map_err(|error| error.to_string())?;
    let response = request.send().await.map_err(|error| error.to_string())?;
    if response.status() == 401 {
        super::account::notify_session_expired();
    }
    response
        .ok()
        .then_some(())
        .ok_or_else(|| format!("Позиция не сохранена: HTTP {}.", response.status()))
}

fn annotation_overlays(annotations: &[Annotation]) -> Vec<serde_json::Value> {
    annotations
        .iter()
        .filter_map(|annotation| {
            let SourceLocator::Pdf(locator) = annotation.anchor.source_locator.as_ref()? else {
                return None;
            };
            Some(json!({
                "pageIndex": locator.page_index,
                "kind": match annotation.kind {
                    AnnotationKind::Highlight { style: HighlightStyle::Bold } => "highlight-bold",
                    AnnotationKind::Highlight { style: HighlightStyle::Green } => "highlight-green",
                    AnnotationKind::Highlight { style: HighlightStyle::Blue } => "highlight-blue",
                    AnnotationKind::Highlight { style: HighlightStyle::Yellow } => "highlight",
                    AnnotationKind::Note { .. } => "note",
                    AnnotationKind::VoiceNote { .. } => "voice",
                },
                "rects": locator.page_rects,
            }))
        })
        .collect()
}

fn update_annotation_overlays(annotations: &[Annotation]) {
    if let Ok(payload) = serde_json::to_string(&annotation_overlays(annotations)) {
        call_pdf_two(
            "setAnnotationsJson",
            JsValue::from_str(PDF_CONTAINER_ID),
            JsValue::from_str(&payload),
        );
    }
}

fn pdf_annotation_page(annotation: &Annotation) -> Option<u32> {
    match annotation.anchor.source_locator.as_ref()? {
        SourceLocator::Pdf(locator) => Some(locator.page_index),
        _ => None,
    }
}

fn annotation_kind_label(kind: &AnnotationKind) -> &'static str {
    match kind {
        AnnotationKind::Highlight {
            style: HighlightStyle::Bold,
        } => "Жирное выделение",
        AnnotationKind::Highlight { .. } => "Выделение",
        AnnotationKind::Note { .. } => "Заметка",
        AnnotationKind::VoiceNote { .. } => "Голосовая заметка",
    }
}

fn parse_tags(value: &str) -> Vec<String> {
    let mut tags = Vec::new();
    for candidate in value
        .split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
    {
        let key = candidate.to_lowercase();
        if !tags.iter().any(|tag: &String| tag.to_lowercase() == key) {
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

fn pdf_api_function(name: &str) -> Result<(JsValue, js_sys::Function), String> {
    let window = web_sys::window().ok_or_else(|| "Browser window недоступен.".to_owned())?;
    let api = js_sys::Reflect::get(window.as_ref(), &JsValue::from_str("LumiPdfReader"))
        .map_err(js_error)?;
    if api.is_null() || api.is_undefined() {
        return Err("PDF.js ещё не загружен.".to_owned());
    }
    let function = js_sys::Reflect::get(&api, &JsValue::from_str(name))
        .map_err(js_error)?
        .dyn_into::<js_sys::Function>()
        .map_err(|_| format!("PDF.js API не содержит метод {name}."))?;
    Ok((api, function))
}

async fn wait_for_pdf_api() -> Result<(), String> {
    for _ in 0..100 {
        if pdf_api_function("mountJson").is_ok() {
            return Ok(());
        }
        browser_delay(50).await;
    }
    Err("PDF.js не загрузился за отведённое время.".to_owned())
}

fn call_pdf_one(name: &str, value: JsValue) -> Result<JsValue, String> {
    let (api, function) = pdf_api_function(name)?;
    function.call1(&api, &value).map_err(js_error)
}

fn call_pdf_two(name: &str, first: JsValue, second: JsValue) {
    if let Ok((api, function)) = pdf_api_function(name) {
        let _ = function.call2(&api, &first, &second);
    }
}

fn destroy_pdf() {
    let _ = call_pdf_one("destroy", JsValue::from_str(PDF_CONTAINER_ID));
}

fn clear_browser_selection() {
    if let Some(selection) =
        web_sys::window().and_then(|window| window.get_selection().ok().flatten())
    {
        let _ = selection.remove_all_ranges();
    }
}

fn js_error(value: JsValue) -> String {
    value
        .as_string()
        .unwrap_or_else(|| "Неизвестная ошибка browser API.".to_owned())
}

async fn browser_delay(milliseconds: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            let _ = window
                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, milliseconds);
        } else {
            let _ = resolve.call0(&JsValue::NULL);
        }
    });
    let _ = JsFuture::from(promise).await;
}
