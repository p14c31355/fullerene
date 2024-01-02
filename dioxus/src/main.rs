use dioxus::prelude::*;
use std::thread::Scope;

fn main() {
    dioxus_web::launch(app);
}

fn app(cx: Scope) -> Element {
    cx.render(rsx! (
        div { "Hello, world!" }
    ))
}