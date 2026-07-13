//! Dioxus/DOM adapter for the shared Stage 4 reader contracts.

use std::cell::RefCell;
use std::collections::HashMap;

use dioxus::prelude::*;
use gloo_net::http::Request;
use lumi_core::{
    LibraryEntry, MoveReadingPositionCommand, PageBoundary, PageFragment, PageMap,
    ReaderNavigation, ReaderPage, ReaderSettings, ReaderTheme, ReaderWidth, ReadingDocument,
    ReadingLink, ReadingLinkKind, ReadingProgress, RenderBlock, RenderPlan, TextRange,
    UpdateReaderSettingsCommand,
};
use uuid::Uuid;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{HtmlElement, RequestCredentials};

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
    footnote: Option<ReadingLink>,
}

#[derive(Clone)]
enum ReaderState {
    Loading,
    Ready(Box<ReaderView>),
    Failed(String),
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
    use_effect(move || {
        spawn(async move {
            match load_reader(material_id).await {
                Ok((entry, document, settings, progress)) => {
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
                                footnote: None,
                            })));
                        }
                        Err(error) => state.set(ReaderState::Failed(error)),
                    }
                }
                Err(error) => state.set(ReaderState::Failed(error)),
            }
        });
    });

    let snapshot = state.read().clone();
    match snapshot {
        ReaderState::Loading => rsx! {
            main { class: "reader-loading", aria_label: "Загрузка книги", aria_live: "polite",
                span { class: "loading-mark", aria_hidden: "true" }
                h1 { "Готовим страницы…" }
                p { "Lumi загружает нормализованный документ и измеряет раскладку в браузере." }
            }
        },
        ReaderState::Failed(error) => rsx! {
            main { class: "reader-loading", aria_label: "Ошибка reader",
                p { class: "eyebrow", "Reader unavailable" }
                h1 { "Не удалось открыть книгу" }
                p { class: "library-alert", role: "alert", "{error}" }
                button { class: "secondary-action", r#type: "button", onclick: move |_| on_close.call(()), "Вернуться в библиотеку" }
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
            rsx! {
                main {
                    class: "reader-workspace {theme_class}",
                    aria_label: "Чтение {title}",
                    style: "--reader-font-size: {view.settings.font_size_px}px; --reader-line-height: {view.settings.line_height_percent};",
                    header { class: "reader-topbar",
                        button { class: "reader-back", r#type: "button", onclick: move |_| on_close.call(()), "← Библиотека" }
                        div { class: "reader-title",
                            strong { "{title}" }
                            span { "{creators}" }
                        }
                        div { class: "reader-tools", aria_label: "Инструменты чтения",
                            button { r#type: "button", aria_pressed: view.toc_open, onclick: move |_| {
                                if let ReaderState::Ready(current) = &mut *state.write() { current.toc_open = !current.toc_open; }
                            }, "Оглавление" }
                            button { r#type: "button", aria_pressed: view.settings_open, onclick: move |_| {
                                if let ReaderState::Ready(current) = &mut *state.write() { current.settings_open = !current.settings_open; }
                            }, "Настройки" }
                        }
                    }

                    div { class: "reader-layout",
                        if view.toc_open {
                            nav { class: "reader-drawer toc-drawer", aria_label: "Оглавление книги",
                                div { class: "drawer-heading",
                                    h2 { "Оглавление" }
                                    button { r#type: "button", aria_label: "Закрыть оглавление", onclick: move |_| {
                                        if let ReaderState::Ready(current) = &mut *state.write() { current.toc_open = false; }
                                    }, "×" }
                                }
                                ol {
                                    for item in view.document.navigation.clone() {
                                        li { button { r#type: "button", onclick: move |_| jump_to_path(state, &item.target_path, csrf, progress_generation), "{item.label}" } }
                                    }
                                }
                            }
                        }

                        section { class: "reader-stage {width_class}", aria_label: "Страница книги",
                            div { class: "reader-history", aria_label: "История переходов",
                                button { r#type: "button", aria_label: "Назад по истории", disabled: !view.navigation.can_go_back(), onclick: move |_| {
                                    if let ReaderState::Ready(current) = &mut *state.write() { current.navigation.go_back(); persist_current(current, csrf, progress_generation); }
                                }, "↶" }
                                button { r#type: "button", aria_label: "Вперёд по истории", disabled: !view.navigation.can_go_forward(), onclick: move |_| {
                                    if let ReaderState::Ready(current) = &mut *state.write() { current.navigation.go_forward(); persist_current(current, csrf, progress_generation); }
                                }, "↷" }
                            }
                            article { class: "reader-page-surface", aria_label: "Страница {current_page + 1} из {page_count}",
                                if let Some(page) = page {
                                    for fragment in page.fragments {
                                        if let Some(block) = view.plan.block(&fragment.node_path).cloned() {
                                            RenderedFragment { block, range: fragment.range, revision_id: view.document.revision_id, on_link: move |link: ReadingLink| activate_link(state, link, csrf, progress_generation) }
                                        }
                                    }
                                }
                            }
                            footer { class: "reader-pagination", aria_label: "Навигация по страницам",
                                button { r#type: "button", disabled: current_page == 0, onclick: move |_| move_page(state, current_page.saturating_sub(1), csrf, progress_generation), "← Назад" }
                                div {
                                    span { "{current_page + 1} / {page_count}" }
                                    progress { max: "{page_count}", value: "{current_page + 1}", aria_label: "Прогресс чтения" }
                                }
                                button { r#type: "button", disabled: current_page + 1 >= page_count, onclick: move |_| move_page(state, current_page + 1, csrf, progress_generation), "Дальше →" }
                            }
                        }

                        if view.settings_open {
                            aside { class: "reader-drawer settings-drawer", aria_label: "Настройки чтения",
                                div { class: "drawer-heading",
                                    h2 { "Настройки" }
                                    button { r#type: "button", aria_label: "Закрыть настройки", onclick: move |_| {
                                        if let ReaderState::Ready(current) = &mut *state.write() { current.settings_open = false; }
                                    }, "×" }
                                }
                                fieldset {
                                    legend { "Тема" }
                                    label { input { r#type: "radio", name: "theme", checked: view.settings.theme == ReaderTheme::Paper, onchange: move |_| update_settings(state, csrf, settings_generation, |settings| settings.theme = ReaderTheme::Paper) } "Бумага" }
                                    label { input { r#type: "radio", name: "theme", checked: view.settings.theme == ReaderTheme::Night, onchange: move |_| update_settings(state, csrf, settings_generation, |settings| settings.theme = ReaderTheme::Night) } "Ночь" }
                                }
                                label { class: "settings-control",
                                    span { "Размер текста: {view.settings.font_size_px}px" }
                                    input { r#type: "range", min: "15", max: "30", value: "{view.settings.font_size_px}", aria_label: "Размер текста", oninput: move |event| {
                                        if let Ok(value) = event.value().parse::<u16>() { update_settings(state, csrf, settings_generation, |settings| settings.font_size_px = value); }
                                    } }
                                }
                                fieldset {
                                    legend { "Ширина строки" }
                                    label { input { r#type: "radio", name: "width", checked: view.settings.width == ReaderWidth::Narrow, onchange: move |_| update_settings(state, csrf, settings_generation, |settings| settings.width = ReaderWidth::Narrow) } "Узкая" }
                                    label { input { r#type: "radio", name: "width", checked: view.settings.width == ReaderWidth::Balanced, onchange: move |_| update_settings(state, csrf, settings_generation, |settings| settings.width = ReaderWidth::Balanced) } "Средняя" }
                                    label { input { r#type: "radio", name: "width", checked: view.settings.width == ReaderWidth::Wide, onchange: move |_| update_settings(state, csrf, settings_generation, |settings| settings.width = ReaderWidth::Wide) } "Широкая" }
                                }
                                p { class: "settings-note", "Настройки сохраняются для аккаунта. Карта страниц пересчитывается на этом устройстве." }
                            }
                        }
                    }

                    if let Some(link) = view.footnote {
                        dialog { class: "footnote-dialog", open: true, aria_label: "Сноска",
                            p { class: "eyebrow", "Примечание" }
                            if let Some(block) = view.plan.block(&link.target_path) {
                                p { "{block.text.clone().unwrap_or_else(|| link.label.clone())}" }
                            } else {
                                p { "{link.label}" }
                            }
                            button { class: "primary-action", r#type: "button", onclick: move |_| {
                                if let ReaderState::Ready(current) = &mut *state.write() { current.footnote = None; }
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
    on_link: EventHandler<ReadingLink>,
) -> Element {
    let text = block.text.as_deref().unwrap_or_default();
    let visible = scalar_slice(text, range.start, range.end);
    let continued = range.start > 0;
    let content = rsx! {
        if continued { span { class: "continued-marker", aria_hidden: "true", "…" } }
        "{visible}"
        for link in block.links.iter().filter(|link| ranges_intersect(link.text_range, range)).cloned() {
            button { class: "inline-link", r#type: "button", onclick: move |_| on_link.call(link.clone()), "Перейти: {link.label}" }
        }
    };
    match block.kind {
        lumi_core::ReadingNodeKind::Heading { .. } => {
            rsx! { h2 { id: "{block.node_id}", class: "reading-heading", {content} } }
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

fn move_page(
    mut state: Signal<ReaderState>,
    page: usize,
    csrf: Signal<String>,
    generation: Signal<u64>,
) {
    if let ReaderState::Ready(current) = &mut *state.write() {
        current
            .navigation
            .move_to(page, current.page_map.pages.len());
        persist_current(current, csrf, generation);
    }
}

fn jump_to_path(
    mut state: Signal<ReaderState>,
    path: &[String],
    csrf: Signal<String>,
    generation: Signal<u64>,
) {
    if let ReaderState::Ready(current) = &mut *state.write() {
        if let Some(page) = current.page_map.page_for_path(path) {
            current
                .navigation
                .jump_to(page, current.page_map.pages.len());
            persist_current(current, csrf, generation);
        }
    }
}

fn activate_link(
    mut state: Signal<ReaderState>,
    link: ReadingLink,
    csrf: Signal<String>,
    generation: Signal<u64>,
) {
    if let ReaderState::Ready(current) = &mut *state.write() {
        if link.kind == ReadingLinkKind::Footnote {
            current.footnote = Some(link);
        } else if let Some(page) = current.page_map.page_for_path(&link.target_path) {
            current
                .navigation
                .jump_to(page, current.page_map.pages.len());
            persist_current(current, csrf, generation);
        }
    }
}

fn update_settings(
    mut state: Signal<ReaderState>,
    csrf: Signal<String>,
    mut generation: Signal<u64>,
    update: impl FnOnce(&mut ReaderSettings),
) {
    if let ReaderState::Ready(current) = &mut *state.write() {
        let current_path = current
            .page_map
            .pages
            .get(current.navigation.current())
            .map(|page| page.start.node_path.clone());
        update(&mut current.settings);
        current.settings = current.settings.normalized();
        if let Ok(page_map) = browser_page_map(&current.plan, current.settings) {
            let restored = current_path
                .as_deref()
                .and_then(|path| page_map.page_for_path(path))
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
        spawn(async move {
            browser_delay(180).await;
            if generation() == next_generation {
                let _ = save_settings(settings, &csrf_token).await;
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

fn persist_current(current: &ReaderView, csrf: Signal<String>, mut generation: Signal<u64>) {
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
    spawn(async move {
        browser_delay(120).await;
        if generation() == next_generation {
            let _ = save_progress(command, &csrf_token).await;
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
    Ok((entry, document, settings, progress))
}

async fn get_json<T: for<'de> serde::Deserialize<'de>>(path: &str) -> Result<T, String> {
    let response = Request::get(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(|error| format!("Сеть/API недоступны: {error}"))?;
    if !response.ok() {
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
        lumi_core::ReadingNodeKind::Heading { .. } => "h2",
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
