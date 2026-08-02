use dioxus::prelude::*;

#[cfg(target_arch = "wasm32")]
mod account;
#[cfg(target_arch = "wasm32")]
mod ai;
#[cfg(target_arch = "wasm32")]
mod community;
#[cfg(target_arch = "wasm32")]
mod desk;
#[cfg(target_arch = "wasm32")]
mod learning;
#[cfg(target_arch = "wasm32")]
mod pdf_reader;
#[cfg(target_arch = "wasm32")]
mod reader;
#[cfg(target_arch = "wasm32")]
mod routing;
#[cfg(target_arch = "wasm32")]
mod search_ui;
#[cfg(target_arch = "wasm32")]
mod voice;

fn main() {
    #[cfg(target_arch = "wasm32")]
    console_error_panic_hook::set_once();
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = asset!("/assets/vendor/pdfjs", AssetOptions::folder());
        let _ = asset!("/assets/styles", AssetOptions::folder());
        use_effect(|| {
            if let Some(root) = web_sys::window()
                .and_then(|window| window.document())
                .and_then(|document| document.document_element())
            {
                let _ = root.set_attribute("lang", "ru");
            }
        });
        rsx! {
            document::Stylesheet { href: asset!("/assets/main.css") }
            document::Script {
                src: asset!(
                    "/assets/pdf-reader.js",
                    AssetOptions::js().with_module(true)
                ),
                r#type: "module",
            }
            document::Meta { name: "theme-color", content: "#f4f0e8" }
            document::Meta {
                name: "viewport",
                content: "width=device-width, initial-scale=1, viewport-fit=cover",
            }
            document::Meta { name: "mobile-web-app-capable", content: "yes" }
            document::Meta { name: "apple-mobile-web-app-capable", content: "yes" }
            document::Meta { name: "apple-mobile-web-app-status-bar-style", content: "default" }
            document::Link { rel: "manifest", href: "/manifest.webmanifest" }
            document::Link { rel: "icon", href: "/icons/favicon.svg", r#type: "image/svg+xml" }
            document::Link { rel: "apple-touch-icon", href: "/icons/apple-touch-icon.png" }
            document::Script { src: "/pwa.js", r#type: "module" }
            account::AccountGate {}
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        rsx! {
            main { class: "app-shell", aria_label: "Lumi web",
                p { "Lumi Web запускается в browser target." }
            }
        }
    }
}
