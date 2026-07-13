use dioxus::prelude::*;
use lumi_core::{
    import_epub_fixture, rich_epub_fixture, sample_fixture_highlight, Annotation, AnnotationExport,
    AnnotationKind, DiagnosticSeverity, HighlightStyle, ImportDiagnostic, ImportedFixture, Job,
    JobStatus, Material, ReadingDocument, ReadingNode, ReadingNodeKind, UserId, API_VERSION,
};

#[cfg(target_arch = "wasm32")]
mod account;

fn main() {
    #[cfg(target_arch = "wasm32")]
    console_error_panic_hook::set_once();
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    let imported = import_epub_fixture(UserId::now_v7(), &rich_epub_fixture());

    rsx! {
        main { class: "app-shell", aria_label: "Lumi S1 web EPUB reader",
            match imported {
                Ok(imported) => {
                    #[cfg(target_arch = "wasm32")]
                    {
                        rsx! { account::AccountGate { imported } }
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        rsx! { S1Workspace { imported } }
                    }
                },
                Err(error) => rsx! {
                    section { class: "empty-state", aria_label: "Import failed",
                        p { class: "eyebrow", "Import failed" }
                        h1 { "Fixture unavailable" }
                        p { class: "reader-error", "{error}" }
                    }
                },
            }
        }
    }
}

#[component]
fn S1Workspace(imported: ImportedFixture) -> Element {
    let material = imported.material.clone();
    let document = imported.reading_document.clone();
    let job = imported.job.clone();
    let annotations = sample_annotations(&imported);
    let export = AnnotationExport::for_material(&material, &annotations);
    let diagnostics = sample_failed_diagnostics();

    rsx! {
        div { class: "workspace",
            aside { class: "sidebar", aria_label: "Project navigation",
                div { class: "brand",
                    p { class: "eyebrow", "S1 web EPUB reader" }
                    h1 { "Lumi" }
                    p { "Personal reading over source-backed EPUB materials." }
                }
                nav { aria_label: "Primary navigation",
                    ul { class: "nav-list",
                        li { a { href: "#library", "Library" } }
                        li { a { href: "#reader", "Reader" } }
                        li { a { href: "#annotations", "Annotations" } }
                        li { a { href: "#diagnostics", "Diagnostics" } }
                    }
                }
            }

            section { class: "main-surface", aria_labelledby: "reader-title",
                header { class: "main-header",
                    div {
                        p { class: "label", "Account cloud replica" }
                        h2 { id: "reader-title", "Web EPUB reader" }
                    }
                    span { class: "api-chip", "API {API_VERSION}" }
                }

                LibraryPanel { material: material.clone(), job: job.clone() }
                ReaderPanel { document, material: material.clone() }
            }

            aside { class: "context-panel", aria_label: "Reader side panels",
                AnnotationPanel { annotations }
                ExportPanel { export }
                DiagnosticsPanel { diagnostics }
            }
        }
    }
}

#[component]
fn LibraryPanel(material: Material, job: Job) -> Element {
    let status = job_status_label(job.status);

    rsx! {
        section { id: "library", class: "library-panel", aria_label: "Library",
            header { class: "panel-heading",
                div {
                    p { class: "eyebrow", "Library" }
                    h3 { "{material.display_title()}" }
                }
                span { class: "status-pill success", "{status}" }
            }
            dl { class: "metadata-grid", aria_label: "Library material metadata",
                div {
                    dt { "Material" }
                    dd { "{material.id}" }
                }
                div {
                    dt { "Revision" }
                    dd { "{material.active_revision_id}" }
                }
                div {
                    dt { "Source" }
                    dd { "{material.source_identity.source_name}" }
                }
                div {
                    dt { "State" }
                    dd { "{material.library_state:?}" }
                }
            }
            div { class: "command-row", aria_label: "Library commands",
                a { class: "command-button", href: "#reader", "Open" }
                button { class: "command-button", r#type: "button", "Archive" }
                button { class: "command-button", r#type: "button", "Download source" }
            }
        }
    }
}

#[component]
fn ReaderPanel(document: ReadingDocument, material: Material) -> Element {
    let navigation = document.navigation.clone();
    let nodes = document.nodes.clone();

    rsx! {
        section { id: "reader", class: "reader-frame", aria_label: "Reader",
            header { class: "reader-document-header",
                div {
                    p { class: "eyebrow", "Imported EPUB" }
                    h3 { "{document.title}" }
                    p { class: "byline", "{document.creators.join(\", \")}" }
                }
                dl { class: "metadata-list", aria_label: "Reading state",
                    div {
                        dt { "Resume" }
                        dd { "Chapter 1, page 1" }
                    }
                    div {
                        dt { "Progress" }
                        dd { "34%" }
                    }
                    div {
                        dt { "Owner" }
                        dd { "{material.owner_id}" }
                    }
                }
            }

            div { class: "reader-toolbar", aria_label: "Reader controls",
                nav { class: "toc-strip", aria_label: "Table of contents",
                    for (index, item) in navigation.into_iter().enumerate() {
                        a { href: "#page-{index}", "{item.label}" }
                    }
                }
                div { class: "settings-group", aria_label: "Typography and theme settings",
                    label {
                        span { "Size" }
                        input { r#type: "range", min: "90", max: "120", value: "104", aria_label: "Font size" }
                    }
                    label {
                        input { r#type: "radio", name: "theme", checked: true, aria_label: "Paper theme" }
                        span { "Paper" }
                    }
                    label {
                        input { r#type: "radio", name: "theme", aria_label: "Night theme" }
                        span { "Night" }
                    }
                }
                div { class: "page-controls", aria_label: "Page navigation",
                    button { class: "icon-button", r#type: "button", disabled: true, aria_label: "Previous page", "<" }
                    span { "Page 1 / {nodes.len()}" }
                    button { class: "icon-button", r#type: "button", aria_label: "Next page", ">" }
                }
            }

            div { class: "reader-content", aria_label: "Reading document",
                for (index, node) in nodes.into_iter().enumerate() {
                    section { id: "page-{index}", class: "reader-page", aria_label: "Reader page",
                        ReaderNode { node }
                    }
                }
            }
        }
    }
}

#[component]
fn AnnotationPanel(annotations: Vec<Annotation>) -> Element {
    rsx! {
        section { id: "annotations", class: "side-section", aria_label: "Notes and highlights",
            header { class: "side-heading",
                p { class: "eyebrow", "Annotations" }
                h3 { "Notes and highlights" }
            }
            div { class: "annotation-list",
                for annotation in annotations {
                    AnnotationItem { annotation }
                }
            }
        }
    }
}

#[component]
fn AnnotationItem(annotation: Annotation) -> Element {
    match annotation.kind {
        AnnotationKind::Highlight { style } => {
            let style_label = highlight_style_label(style);

            rsx! {
                article { class: "annotation-item highlight",
                    p { class: "annotation-kind", "{style_label} highlight" }
                    blockquote { "{annotation.anchor.quote}" }
                }
            }
        }
        AnnotationKind::Note { body } => rsx! {
            article { class: "annotation-item note",
                p { class: "annotation-kind", "Note" }
                blockquote { "{annotation.anchor.quote}" }
                p { class: "note-body", "{body}" }
            }
        },
    }
}

#[component]
fn ExportPanel(export: AnnotationExport) -> Element {
    let entry_count = export.entries.len();
    let first_quote = export
        .entries
        .first()
        .map(|entry| entry.quote.as_str())
        .unwrap_or("No annotations yet");

    rsx! {
        section { id: "export", class: "side-section", aria_label: "Annotation export",
            header { class: "side-heading",
                p { class: "eyebrow", "Export" }
                h3 { "Portable JSON" }
            }
            dl { class: "metadata-list compact", aria_label: "Export metadata",
                div {
                    dt { "Entries" }
                    dd { "{entry_count}" }
                }
                div {
                    dt { "Source" }
                    dd { "{export.source.source_name}" }
                }
            }
            blockquote { "{first_quote}" }
        }
    }
}

#[component]
fn DiagnosticsPanel(diagnostics: Vec<ImportDiagnostic>) -> Element {
    rsx! {
        section { id: "diagnostics", class: "side-section", aria_label: "Import diagnostics",
            header { class: "side-heading",
                p { class: "eyebrow", "Diagnostics" }
                h3 { "Failed import sample" }
            }
            ul { class: "diagnostic-list",
                for diagnostic in diagnostics {
                    li {
                        span { class: "status-pill danger", "{diagnostic_label(diagnostic.severity)}" }
                        strong { "{diagnostic.code}" }
                        p { "{diagnostic.message}" }
                    }
                }
            }
        }
    }
}

#[component]
fn ReaderNode(node: ReadingNode) -> Element {
    match node.kind {
        ReadingNodeKind::Section => rsx! {
            section { class: "reading-section", "data-node-id": "{node.id}",
                for child in node.children {
                    ReaderNode { node: child }
                }
            }
        },
        ReadingNodeKind::Heading { level } => {
            let text = node.text.unwrap_or_default();
            match level {
                1 => {
                    rsx! { h4 { class: "reading-heading", "data-node-id": "{node.id}", "{text}" } }
                }
                _ => {
                    rsx! { h5 { class: "reading-heading", "data-node-id": "{node.id}", "{text}" } }
                }
            }
        }
        ReadingNodeKind::Paragraph => {
            let text = node.text.unwrap_or_default();
            rsx! { p { class: "reading-paragraph", "data-node-id": "{node.id}", "{text}" } }
        }
        ReadingNodeKind::Image => {
            let text = node.text.unwrap_or_default();
            let resource = node
                .resource_hash
                .unwrap_or_else(|| "missing-resource".to_owned());
            rsx! {
                figure { class: "reading-figure", "data-node-id": "{node.id}",
                    div { class: "image-placeholder", aria_label: "{text}" }
                    figcaption { "{text}" }
                    code { "{resource}" }
                }
            }
        }
        ReadingNodeKind::Caption => {
            let text = node.text.unwrap_or_default();
            rsx! { p { class: "reading-caption", "data-node-id": "{node.id}", "{text}" } }
        }
        ReadingNodeKind::Footnote => {
            let text = node.text.unwrap_or_default();
            rsx! { aside { class: "reading-footnote", "data-node-id": "{node.id}", "{text}" } }
        }
        ReadingNodeKind::PluginPlaceholder { capability } => rsx! {
            aside { class: "reading-plugin", "data-node-id": "{node.id}",
                "{capability}"
            }
        },
    }
}

fn sample_annotations(imported: &ImportedFixture) -> Vec<Annotation> {
    let Some(highlight_command) = sample_fixture_highlight(imported) else {
        return Vec::new();
    };

    let mut note_command = highlight_command.clone();
    note_command.kind = AnnotationKind::Note {
        body: "Capture the anchor model for reimport recovery.".to_owned(),
    };

    vec![
        Annotation::create(highlight_command, 0),
        Annotation::create(note_command, 0),
    ]
}

fn sample_failed_diagnostics() -> Vec<ImportDiagnostic> {
    vec![ImportDiagnostic {
        severity: DiagnosticSeverity::Error,
        code: "epub_fixture_import_failed".to_owned(),
        message: "EPUB fixture must contain at least one readable section".to_owned(),
        source_path: Some("empty.epub".to_owned()),
    }]
}

fn job_status_label(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Queued => "Queued",
        JobStatus::Running => "Running",
        JobStatus::Succeeded => "Imported",
        JobStatus::Failed => "Failed",
    }
}

fn highlight_style_label(style: HighlightStyle) -> &'static str {
    match style {
        HighlightStyle::Yellow => "Yellow",
        HighlightStyle::Green => "Green",
        HighlightStyle::Blue => "Blue",
    }
}

fn diagnostic_label(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Info => "Info",
        DiagnosticSeverity::Warning => "Warning",
        DiagnosticSeverity::Error => "Error",
    }
}
