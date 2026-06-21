use dioxus::prelude::*;
use lumi_core::API_VERSION;

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
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
                        p { class: "eyebrow", "Current scaffold" }
                        h3 { "Source-backed reader surface" }
                        p {
                            "This shell keeps the first web target visible to Playwright while the real reader core, import pipeline and account flows are implemented."
                        }
                        div { class: "reader-grid",
                            article { id: "library", class: "reader-tile",
                                strong { "Material" }
                                span { "Material and DocumentRevision contracts live in shared Rust crates." }
                            }
                            article { class: "reader-tile",
                                strong { "ReadingDocument" }
                                span { "Dioxus renders platform UI, not the platform-independent reader core." }
                            }
                            article { id: "knowledge", class: "reader-tile",
                                strong { "Anchors" }
                                span { "Notes and highlights must resolve through source-backed anchors." }
                            }
                        }
                    }
                }

                aside { id: "jobs", class: "context-panel", aria_label: "Development status",
                    h2 { "Development status" }
                    ul { class: "status-list",
                        li {
                            strong { "API boundary" }
                            span { "Axum owns /api/v1 contracts." }
                        }
                        li {
                            strong { "Browser verification" }
                            span { "Playwright uses semantic roles and labels." }
                        }
                        li {
                            strong { "Next slice" }
                            span { "S0 domain contracts, import fixture and reader core." }
                        }
                    }
                }
            }
        }
    }
}
