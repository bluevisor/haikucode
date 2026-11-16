use super::helpers::clean_text;
use super::palette::palette_by_name;
use super::{
    echo_excerpt, palette_for, want_mouse_capture, App, ConfigPatch, Handles, KeyCode, KeyEvent,
    KeyModifiers, Mode, ProviderDraft, ProviderStep, MouseAction, MouseButton, MouseEventKind, SelectKind, ECHO_LINES,
    TAB_W, THEMES, UiConfig, UiEvent, WorkerCmd,
};

mod transcript;
mod provider;
mod commands;
mod composer;
mod mouse;

/// Press Enter on the picker row containing `needle`.
fn pick_row(app: &mut App, h: &Handles, needle: &str) {
    let p = app.picker.as_mut().expect("a picker is open");
    p.cursor = p.items.iter().position(|i| i.contains(needle)).unwrap_or_else(|| panic!("no row {needle:?} in {:?}", p.items));
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), h);
}

/// What the worker was sent, in a few words each.
fn sent_kinds(rx: &std::sync::mpsc::Receiver<WorkerCmd>) -> Vec<String> {
    rx.try_iter()
        .map(|c| match c {
            WorkerCmd::Patch(ConfigPatch::AddProvider(c)) => format!("add {} {} [{}]", c.name, c.base_url, c.model),
            WorkerCmd::Patch(ConfigPatch::Provider { provider, .. }) => format!("switch {provider}"),
            WorkerCmd::Patch(ConfigPatch::ProviderKey { provider, key }) => format!("key {provider} {key}"),
            WorkerCmd::Patch(ConfigPatch::ProviderAuth { provider, mode }) => format!("auth {provider} {mode}"),
            WorkerCmd::Patch(ConfigPatch::SignOut(p)) => format!("signout {p}"),
            WorkerCmd::Login(p) => format!("login {p}"),
            WorkerCmd::SetModel(m) => format!("model {m}"),
            WorkerCmd::ListModels => "models".into(),
            _ => "other".into(),
        })
        .collect()
}

/// A `Handles` backed by throwaway channels, plus the receiver so a test
/// can assert what the UI sent to the worker.
fn test_handles() -> (Handles, std::sync::mpsc::Receiver<WorkerCmd>) {
    use std::sync::atomic::{AtomicBool, AtomicU8};
    use std::sync::mpsc;
    use std::sync::Arc;

    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (appr_tx, _appr_rx) = mpsc::channel();
    let h = Handles {
        join: std::thread::spawn(|| {}),
        cmd_tx,
        appr_tx,
        shared: crate::agent::Shared {
            cancel: Arc::new(AtomicBool::new(false)),
            perm: Arc::new(AtomicU8::new(0)),
        },
    };
    (h, cmd_rx)
}

/// A `UiConfig` with test-sized numbers; `price_*` are zeroed so costs are
/// deterministic unless a test sets them.
fn test_ui_config() -> UiConfig {
    use std::sync::atomic::AtomicU8;
    use std::sync::Arc;

    UiConfig {
        model: "m0".into(),
        theme: "Rainbow".into(),
        ascii: false,
        ctx_limit: 128_000,
        price_in: 0.0,
        price_out: 0.0,
        price_cached_in: None,
        price_currency: crate::money::Currency::default(),
        cost_known: true,
        perm: Arc::new(AtomicU8::new(0)),
        settings: crate::config::Config::default(),
    }
}

/// The real key path (`App::on_key` → `Mode::Select`) used by `/model`:
/// a mis-guarded match arm silently swallowed plain Up/Down while the
/// Ctrl+j/k aliases still worked.
fn select_app(models: &[&str]) -> (App, Handles, std::sync::mpsc::Receiver<WorkerCmd>) {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, cmd_rx) = test_handles();
    app.handle_event(
        UiEvent::ModelList(models.iter().map(|m| m.to_string()).collect()),
        &h,
    );
    assert!(
        matches!(app.mode, Mode::Select),
        "ModelList opens the picker"
    );
    (app, h, cmd_rx)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Ctrl+arrow: the transcript-scroll gesture.
fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

/// An idle app whose transcript is taller than the viewport, so scrolling
/// has somewhere to go. `max_top`/`view_h` are normally set by the render
/// pass; a unit test pokes them directly.
fn scrollable_app() -> (App, Handles) {
    let mut app = App::new(test_ui_config(), vec!["first".into(), "second".into()]);
    app.max_top = 40;
    app.view_h = 10;
    app.scroll = 40; // pinned to the live end
    let (h, _rx) = test_handles();
    (app, h)
}

fn typed(app: &mut App, h: &Handles, text: &str) {
    for c in text.chars() {
        app.on_key(key(KeyCode::Char(c)), h);
    }
}

/// The real render path: an overflowing transcript, a small screen, and the
/// status bar read back from the backend buffer. Guards against the scroll
/// state being right while nothing visible changes.
fn draw(app: &mut App, term: &mut ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let area = *buf.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
