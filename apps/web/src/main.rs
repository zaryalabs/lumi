use dioxus::prelude::*;
use lumi_core::{
    import_epub_fixture, rich_epub_fixture, sample_fixture_highlight, ImportedFixture, ReadingNode,
    ReadingNodeKind, UserId, API_VERSION,
};

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    let imported = import_epub_fixture(UserId::now_v7(), &rich_epub_fixture());

    rsx! {
        main { class: "app-shell", aria_label: "Lumi development shell",
            div { class: "workspace",
                aside { class: "sidebar", aria_label: "Project navigation",
                    div { class: "brand",
                        p { class: "eyebrow", "S0 architecture skeleton" }
                        h1 { "Lumi" }
                        p { "Reading, annotations and learning over source-backed materials." }
                    }
                    nav { aria_label: "Primary navigation",
                        ul { class: "nav-list",
                            li { a { href: "#library", "Library" } }
                            li { a { href: "#reader", "Reader" } }
                            li { a { href: "#knowledge", "Knowledge" } }
                            li { a { href: "#jobs", "Jobs" } }
                        }
                    }
                }

                section { class: "main-surface", aria_labelledby: "reader-title",
                    header { class: "main-header",
                        div {
                            p { class: "label", "Web shell" }
                            h2 { id: "reader-title", "Reader platform adapter" }
                        }
                        span { class: "api-chip", "API {API_VERSION}" }
                    }

                    section { id: "reader", class: "reader-frame", aria_label: "Reader contract",
                        match imported {
                            Ok(imported) => rsx! { ReaderFixture { imported } },
                            Err(error) => rsx! {
                                p { class: "eyebrow", "Import failed" }
                                h3 { "Fixture unavailable" }
                                p { class: "reader-error", "{error}" }
                            },
                        }
                    }
                }

                aside { id: "jobs", class: "context-panel", aria_label: "Development status",
                    h2 { "Development status" }
                    ul { class: "status-list",
                        li {
                            strong { "API boundary" }
                            span { "/api/v1 materials, revisions, imports, jobs, blobs and reader commands." }
                        }
                        li {
                            strong { "Fixture" }
                            span { "rich.epub -> Material -> DocumentRevision -> ReadingDocument." }
                        }
                        li {
                            strong { "Annotation" }
                            span { "Anchor-backed highlight and note commands are served by Axum." }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ReaderFixture(imported: ImportedFixture) -> Element {
    let document = imported.reading_document.clone();
    let annotation_quote = sample_fixture_highlight(&imported)
        .map(|command| command.anchor.quote)
        .unwrap_or_else(|| "No anchor available".to_owned());

    rsx! {
        div { class: "reader-document",
            header { class: "reader-document-header",
                div {
                    p { class: "eyebrow", "Imported fixture" }
                    h3 { "{document.title}" }
                    p { class: "byline", "{document.creators.join(\", \")}" }
                }
                dl { class: "metadata-list", aria_label: "Material metadata",
                    div {
                        dt { "Material" }
                        dd { "{imported.material.id}" }
                    }
                    div {
                        dt { "Revision" }
                        dd { "{imported.revision.id}" }
                    }
                }
            }

            div { class: "reader-content", aria_label: "Reading document",
                for node in document.nodes {
                    ReaderNode { node }
                }
            }
        }

        aside { id: "knowledge", class: "annotation-panel", aria_label: "Fixture annotations",
            h3 { "Annotation sample" }
            blockquote { "{annotation_quote}" }
            dl { class: "metadata-list compact", aria_label: "Anchor metadata",
                div {
                    dt { "Anchor" }
                    dd { "{imported.revision.id}" }
                }
                div {
                    dt { "Job" }
                    dd { "{imported.job.id}" }
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
