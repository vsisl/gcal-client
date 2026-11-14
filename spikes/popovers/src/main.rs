//! Throwaway spike: popovers and a guest-autocomplete dropdown near the window edge.

use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

slint::include_modules!();

const PEOPLE: [&str; 10] = [
    "Ada Lovelace <ada@example.com>",
    "Alan Turing <alan@example.com>",
    "Anna Novak <anna@example.com>",
    "Grace Hopper <grace@example.com>",
    "Joan Clarke <joan@example.com>",
    "John Backus <john@example.com>",
    "Jonas Svoboda <jonas@example.com>",
    "Katherine Johnson <katherine@example.com>",
    "Linus Torvalds <linus@example.com>",
    "Margaret Hamilton <margaret@example.com>",
];

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let model = Rc::new(VecModel::<SharedString>::default());
    ui.set_suggestions(ModelRc::from(model.clone()));
    ui.on_query_changed(move |query| {
        let query = query.to_lowercase();
        let matches: Vec<SharedString> = PEOPLE
            .iter()
            .filter(|p| !query.is_empty() && p.to_lowercase().contains(&query))
            .take(5)
            .map(|p| SharedString::from(*p))
            .collect();
        model.set_vec(matches);
    });
    ui.run()
}
