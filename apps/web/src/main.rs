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
        rsx! { account::AccountGate {} }
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
