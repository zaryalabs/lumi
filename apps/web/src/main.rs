use dioxus::prelude::*;

#[cfg(target_arch = "wasm32")]
mod account;
#[cfg(target_arch = "wasm32")]
mod reader;

fn main() {
    #[cfg(target_arch = "wasm32")]
    console_error_panic_hook::set_once();
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    #[cfg(target_arch = "wasm32")]
    {
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
            document::Meta { name: "theme-color", content: "#f4f0e8" }
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
