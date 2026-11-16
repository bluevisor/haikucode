use super::*;

#[test]
fn compact_passes_its_focus_hint_to_the_worker() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, rx) = test_handles();
    app.run_command("compact keep the API shapes", &h);
    assert!(matches!(rx.try_recv(), Ok(WorkerCmd::Compact(Some(f))) if f == "keep the API shapes"));
    app.mode = Mode::Idle;
    app.run_command("compact", &h);
    assert!(matches!(rx.try_recv(), Ok(WorkerCmd::Compact(None))));
}

/// The `/` palette: arrows move the highlight (wrapping) instead of
/// scrolling, and Enter on a partial command runs the highlighted one.
#[test]
fn slash_palette_arrows_select_and_enter_runs_the_highlight() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, _rx) = test_handles();
    app.input = "/re".into();
    app.cursor = 3;
    let sugg: Vec<&str> = app.slash_suggestions().iter().map(|(c, _)| *c).collect();
    assert!(sugg.len() >= 2, "{sugg:?}");
    app.on_key(key(KeyCode::Down), &h);
    assert_eq!(app.suggest_idx, 1);
    app.on_key(key(KeyCode::Up), &h);
    app.on_key(key(KeyCode::Up), &h);
    assert_eq!(app.suggest_idx, sugg.len() - 1, "wraps to the last entry");
    app.on_key(key(KeyCode::Enter), &h);
    assert_eq!(app.history.last().map(String::as_str), Some(sugg[sugg.len() - 1]), "ran the highlighted command");
}

/// `/rewind` round trip: the worker's list opens a picker, choosing an
/// entry asks for that turn (0-based), and the undone prompt comes back
/// into the composer for editing.
#[test]
fn rewind_picker_sends_the_chosen_turn_and_refills_the_prompt() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, cmd_rx) = test_handles();
    app.handle_event(
        UiEvent::TurnList(vec!["3. third".into(), "2. second".into(), "1. first".into()]),
        &h,
    );
    assert!(matches!(app.mode, Mode::Select), "TurnList opens the picker");
    app.on_key(key(KeyCode::Down), &h);
    app.on_key(key(KeyCode::Enter), &h);
    let sent: Vec<WorkerCmd> = cmd_rx.try_iter().collect();
    assert!(
        sent.iter().any(|c| matches!(c, WorkerCmd::Rewind(1))),
        "\"2. second\" is turn index 1"
    );
    app.handle_event(UiEvent::Rewound("second".into()), &h);
    assert_eq!(app.input, "second");
    assert_eq!(app.cursor, 6);
    // An empty list (nothing to rewind) leaves no picker behind.
    app.handle_event(UiEvent::TurnList(Vec::new()), &h);
    assert!(app.picker.is_none() && !matches!(app.mode, Mode::Select));
}

/// `/effort high` goes live in the worker and persists; nonsense is refused.
#[test]
fn effort_command_sets_reasoning() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, cmd_rx) = test_handles();
    app.run_command("effort High", &h);
    assert_eq!(app.settings.reasoning, "high");
    let sent: Vec<WorkerCmd> = cmd_rx.try_iter().collect();
    assert!(sent.iter().any(|c| matches!(c, WorkerCmd::Patch(ConfigPatch::Reasoning(r)) if r == "high")));
    app.run_command("effort sideways", &h);
    assert_eq!(app.settings.reasoning, "high", "unchanged by a bad value");
    assert!(cmd_rx.try_iter().next().is_none());
}

/// Typing `/cmd ` lists that command's options in the palette, filtered
/// as you type; Enter on an option needing an argument fills the line
/// rather than running it, and a bare command lists its options.
#[test]
fn slash_commands_offer_their_options() {
    use crate::ui::types::Kind;
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, _cmd_rx) = test_handles();
    for (opt, _) in crate::ui::types::SLASH_OPTIONS {
        let cmd = opt.split(' ').next().unwrap();
        assert!(crate::ui::types::SLASH_COMMANDS.iter().any(|(c, _)| *c == cmd), "{opt} names no command");
    }
    app.input = "/permissions ".into();
    let opts: Vec<&str> = app.slash_suggestions().iter().map(|(c, _)| *c).collect();
    assert_eq!(opts, ["/permissions list", "/permissions allow ", "/permissions deny ", "/permissions remove ", "/permissions reload"]);
    app.input = "/effort x".into();
    assert_eq!(app.slash_suggestions().iter().map(|(c, _)| *c).collect::<Vec<_>>(), ["/effort xhigh"]);
    app.input = "/permissions allow bash(ls:*)".into();
    assert!(app.slash_suggestions().is_empty(), "palette closes once the argument is being typed");

    // Enter on a partial option that needs an argument fills the line.
    app.input = "/permissions al".into();
    app.cursor = app.char_len();
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &h);
    assert_eq!(app.input, "/permissions allow ");

    // A bare command lists its options.
    app.transcript.clear();
    app.run_command("permissions", &h);
    let text: Vec<&str> = app.transcript.iter().filter(|l| l.kind == Kind::Notice).map(|l| l.text.as_str()).collect();
    assert!(text.contains(&"/permissions options:"), "{text:?}");
    assert!(text.iter().any(|t| t.contains("/permissions allow") && t.contains("never ask")), "{text:?}");
}

/// /yolo (and its old name /auto) toggles YOLO on and off, and the mode
/// lands in the settings that get saved for the next session.
#[test]
fn yolo_command_toggles_and_records_the_mode() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, _cmd_rx) = test_handles();
    app.run_command("yolo", &h);
    assert_eq!((app.perm(), app.settings.permission.as_str()), (crate::agent::PERM_AUTO, "yolo"));
    app.run_command("auto", &h);
    assert_eq!((app.perm(), app.settings.permission.as_str()), (crate::agent::PERM_ASK, "ask"));
    app.cycle_perm();
    assert_eq!(app.settings.permission, "yolo", "Shift+Tab is recorded too");
}

#[test]
fn statusline_command_sets_resets_and_rejects_unknown_segments() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, _rx) = test_handles();
    app.cwd_label = "~/proj".into();
    app.run_command("statusline cwd version model", &h);
    assert_eq!(app.statusline, ["cwd", "version", "model"]);
    assert_eq!(app.settings.statusline, ["cwd", "version", "model"]);
    let mut term = Terminal::new(TestBackend::new(100, 16)).unwrap();
    let screen = draw(&mut app, &mut term);
    let row = screen.lines().find(|l| l.contains("~/proj") && l.contains("v0.")).unwrap_or("");
    let (c, v, m) = (row.find("~/proj"), row.find(&format!("v{}", env!("CARGO_PKG_VERSION"))), row.find(&app.model_info));
    assert!(c < v && v < m && m.is_some(), "segments drawn in order:\n{screen}");
    assert!(!row.contains("ctx"), "ctx was left out:\n{screen}");

    app.run_command("statusline model gti", &h);
    assert_eq!(app.statusline, ["cwd", "version", "model"], "a typo changes nothing");

    app.run_command("statusline reset", &h);
    assert_eq!(app.statusline, crate::ui::statusline::DEFAULT);
    assert!(app.settings.statusline.is_empty(), "the default isn't written out");
}

#[test]
fn the_statusline_picker_toggles_reorders_previews_and_cancels() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, _rx) = test_handles();
    app.run_command("statusline", &h);
    assert!(matches!(app.mode, Mode::StatuslineSelect { .. }));
    // Row 0 is model: hide it, and the row previews without it.
    app.on_key(key(KeyCode::Char(' ')), &h);
    assert!(!app.statusline.contains(&"model".to_string()));
    // Move to ctx (row 4) and lift it to the top.
    for _ in 0..4 {
        app.on_key(key(KeyCode::Down), &h);
    }
    for _ in 0..4 {
        app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT), &h);
    }
    assert_eq!(app.statusline[0], "ctx");
    app.do_esc(&h);
    assert_eq!(app.statusline, crate::ui::statusline::DEFAULT, "Esc puts it back");
    assert!(matches!(app.mode, Mode::Idle));

    // Again, and keep it this time.
    app.run_command("statusline", &h);
    for _ in 0..7 {
        app.on_key(key(KeyCode::Down), &h);
    }
    app.on_key(key(KeyCode::Char(' ')), &h); // row 7 is cwd (hidden by default)
    app.on_key(key(KeyCode::Enter), &h);
    assert!(matches!(app.mode, Mode::Idle));
    assert_eq!(app.statusline.last().map(String::as_str), Some("cwd"));
    assert_eq!(app.settings.statusline, app.statusline);
}

#[test]
fn the_last_shown_statusline_item_cannot_be_hidden() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, _rx) = test_handles();
    app.run_command("statusline git", &h);
    app.run_command("statusline", &h);
    app.on_key(key(KeyCode::Char(' ')), &h);
    assert_eq!(app.statusline, ["git"]);
}

/// A balance in another currency is shown as-is (tagged on the status
/// line) with no notice: the old one urged switching the price currency,
/// which only relabels USD prices.
#[test]
fn a_balance_in_another_currency_is_shown_without_a_nudge() {
    let bal = |code: &str| crate::money::Balance {
        amount: "135.70".into(),
        currency: crate::money::Currency::parse(code),
        converted_from: None,
    };
    let (mut app, h) = scrollable_app();
    let before = app.transcript.len();
    app.handle_event(UiEvent::Balance(bal("CNY")), &h);
    assert_eq!(app.transcript.len(), before, "no notice");
    assert_eq!(app.balance.as_ref().unwrap().render(), "¥135.70");
    assert_eq!(app.price_currency.code, "USD", "prices stay USD");
}

#[test]
fn switching_providers_drops_the_old_balance_and_the_ctx_row_shows_the_live_window() {
    let (mut app, h) = scrollable_app();
    app.handle_event(UiEvent::Balance(crate::money::Balance {
        amount: "12.30".into(),
        currency: crate::money::Currency::parse("USD"),
        converted_from: None,
    }), &h);
    assert!(app.balance.is_some());
    app.switch_provider("mylocal".into(), "http://localhost:11434/v1".into(), "qwen3".into(), &h);
    assert!(app.balance.is_none(), "DeepSeek's balance isn't the local server's");
    app.handle_event(UiEvent::Balance(crate::money::Balance {
        amount: "1".into(), currency: crate::money::Currency::parse("USD"), converted_from: None,
    }), &h);
    app.handle_event(UiEvent::BalanceCleared, &h);
    assert!(app.balance.is_none());
    // the /config row shows (and edits from) the window in effect
    app.handle_event(UiEvent::ContextLimit(8192), &h);
    assert_eq!(app.setting_value(9), "8192");
}

#[test]
fn the_config_panel_can_switch_the_price_currency() {
    let (mut app, _h, _cmd_rx) = select_app(&["m0"]);
    app.mode = Mode::Idle;
    app.settings.price_currency = "USD".into();
    let (h2, cmd_rx2) = test_handles();
    let row = crate::ui::types::SETTING_ROWS.iter().position(|&r| r == 11).unwrap();

    app.activate_setting(row, 1, &h2);
    assert_eq!((app.price_currency.code.as_str(), app.settings.price_currency.as_str()), ("CNY", "CNY"));
    assert!(
        matches!(cmd_rx2.try_recv(), Ok(WorkerCmd::Patch(ConfigPatch::PriceCurrency(c))) if c == "CNY"),
        "the worker is told so the balance preference follows"
    );
    app.activate_setting(row, -1, &h2);
    assert_eq!(app.settings.price_currency, "USD");
    app.activate_setting(row, -1, &h2);
    assert_eq!(app.settings.price_currency, "CHF", "the list wraps");
    // A hand-edited code that isn't on the list restarts at USD.
    app.settings.price_currency = "XYZ".into();
    app.activate_setting(row, 1, &h2);
    assert_eq!(app.settings.price_currency, "USD");
}

/// The cost counter follows the provider for new spend only: tokens spent
/// before a switch keep the price they were spent at, and a custom server
/// without a price list prices nothing new (status line and /cost alike).
#[test]
fn the_cost_counter_follows_the_provider() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let (mut app, _h, _cmd_rx) = select_app(&["m0"]);
    app.mode = Mode::Idle;
    let (h, _cmd_rx) = test_handles();
    // test_ui_config zeros the prices; take the default provider's list.
    app.sync_prices();
    app.add_spent(crate::money::Spent { prompt: 100_000, completion: 50_000, ..Default::default() });
    assert!(app.cost_known);
    let deepseek_cost = app.session_cost().unwrap();
    assert!(deepseek_cost > 0.0);

    app.switch_provider(
        "anthropic".into(),
        "https://api.anthropic.com/v1".into(),
        "claude-opus-5-5".into(),
        &h,
    );
    assert!(app.cost_known);
    assert!(
        (app.session_cost().unwrap() - deepseek_cost).abs() < 1e-9,
        "switching doesn't re-price what was spent on DeepSeek"
    );
    // The same tokens again, on Opus, cost far more than they did.
    app.add_spent(crate::money::Spent { prompt: 100_000, completion: 50_000, ..Default::default() });
    let opus_part = app.session_cost().unwrap() - deepseek_cost;
    assert!(opus_part > deepseek_cost + 1.0, "{deepseek_cost} then {opus_part}");
    let before_local = app.session_cost().unwrap();

    app.switch_provider(
        "mylocal".into(),
        "http://localhost:11434/v1".into(),
        "qwen3".into(),
        &h,
    );
    assert!(!app.cost_known, "a custom server has no known price");
    // What was already spent stays; spend here adds nothing to it.
    app.add_spent(crate::money::Spent { prompt: 100_000, ..Default::default() });
    assert!((app.session_cost().unwrap() - before_local).abs() < 1e-9);
    let lines = app.cost_lines();
    assert!(lines[0].starts_with("session: 400.0k tokens (300.0k in / 100.0k out)"), "{lines:?}");

    // A fresh session (no priced spend yet) on that server hides the cost.
    app.reset_session_counters();
    app.add_spent(crate::money::Spent { prompt: 100_000, completion: 50_000, ..Default::default() });
    assert!(app.session_cost().is_none());
    let lines = app.cost_lines();
    assert!(lines[0].starts_with("session: 150.0k tokens (100.0k in / 50.0k out)"), "{lines:?}");
    assert!(!lines[0].contains('·'), "the cost is hidden:\n{lines:?}");
    let mut term = Terminal::new(TestBackend::new(120, 20)).unwrap();
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("150.0k tok"), "tokens stay on the line:\n{screen}");
    assert!(!screen.contains('$'), "no cost figure:\n{screen}");
}

/// Enter on the api key row opens an empty field; saving it empty used to
/// wipe the stored key, and every launch after fell into the setup wizard.
#[test]
fn an_empty_api_key_edit_keeps_the_saved_key() {
    let (mut app, _h, _cmd_rx) = select_app(&["m0"]);
    app.settings.api_key = "sk-keep".into();
    let (h2, cmd_rx2) = test_handles();
    app.commit_setting(3, "   ".into(), &h2);
    assert_eq!(app.settings.api_key, "sk-keep");
    assert!(cmd_rx2.try_recv().is_err(), "nothing is sent to the worker");
    app.commit_setting(3, "sk-new".into(), &h2);
    assert_eq!(app.settings.api_key, "sk-new");
}

/// The row being edited shows the typed text (an api key masked), not the
/// stale value.
#[test]
fn the_config_row_being_edited_shows_what_is_typed() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::new(test_ui_config(), Vec::new());
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let model_row = crate::ui::types::SETTING_ROWS.iter().position(|&r| r == 2).unwrap();
    app.mode = Mode::Settings { cursor: model_row, edit: Some("deepseek-v4-pro".into()) };
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("deepseek-v4-pro"), "{screen}");
    assert!(screen.contains("Enter to save"), "{screen}");
    let key_row = crate::ui::types::SETTING_ROWS.iter().position(|&r| r == 3).unwrap();
    app.mode = Mode::Settings { cursor: key_row, edit: Some("sk-secret".into()) };
    let screen = draw(&mut app, &mut term);
    assert!(!screen.contains("sk-secret") && screen.contains("•••••••••"), "{screen}");
}

/// The status line's two money readouts, as drawn. This is the bug being
/// guarded: a hardcoded `$` on the cost while the balance rendered as `¥`.
#[test]
fn status_line_money_readouts_never_mix_currencies_silently() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let cny = crate::money::Currency::parse("cny");
    let bal = |code: &str, amount: &str| crate::money::Balance {
        amount: amount.into(),
        currency: crate::money::Currency::parse(code),
        converted_from: None,
    };

    // A CNY-billed account (balance ¥135.70) with the default USD price
    // list: both figures must carry their ISO code, because a `$0.0300`
    // next to `¥135.70` looks comparable and isn't.
    let mut app = App::new(test_ui_config(), Vec::new());
    app.price_in = 0.14;
    app.price_out = 0.28;
    app.sess_prompt = 100_000;
    app.sess_completion = 50_000;
    app.balance = Some(bal("CNY", "135.70"));
    let mut term = Terminal::new(TestBackend::new(120, 20)).unwrap();
    let screen = draw(&mut app, &mut term);
    assert!(
        screen.contains("$0.03 USD"),
        "cost is unit-labelled:\n{screen}"
    );
    assert!(
        screen.contains("bal ¥135.70 CNY"),
        "balance is too:\n{screen}"
    );

    // Set the price currency to CNY (and use DeepSeek's CNY list) and the
    // readouts line up in one currency, with no tags.
    app.price_currency = cny.clone();
    app.price_in = 1.0;
    app.price_out = 2.0;
    let screen = draw(&mut app, &mut term);
    assert!(
        screen.contains("¥0.20 · 150.0k tok"),
        "cost in CNY:\n{screen}"
    );
    assert!(screen.contains("bal ¥135.70"), "balance in CNY:\n{screen}");
    assert!(!screen.contains("USD"), "no leftover USD:\n{screen}");

    // A USD account with the default prices reads plainly, as before: no
    // currency tags, because there is nothing to disambiguate.
    app.price_currency = crate::money::Currency::default();
    app.price_in = 0.14;
    app.price_out = 0.28;
    app.balance = Some(bal("USD", "12.34"));
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("$0.03 · 150.0k tok"), "cost:\n{screen}");
    assert!(screen.contains("bal $12.34"), "balance:\n{screen}");

    // A CNY balance converted into the USD status line reads as one
    // currency, marked approximate, with no tags.
    app.balance = bal("CNY", "135.70").convert(&crate::money::Currency::default(), 0.15);
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("bal ≈$20.35"), "converted balance:\n{screen}");
    assert!(!screen.contains("CNY"), "no CNY left on the line:\n{screen}");
    assert!(
        !screen.contains("CNY") && !screen.contains("USD"),
        "untagged when aligned:\n{screen}"
    );

    // Sub-cent sessions keep four decimals so a short turn isn't "$0.00".
    app.sess_prompt = 10_000;
    app.sess_completion = 0;
    let screen = draw(&mut app, &mut term);
    assert!(
        screen.contains("$0.0014 · 10.0k tok"),
        "sub-cent cost:\n{screen}"
    );
}

#[test]
fn model_picker_moves_with_arrows_and_enters_the_highlighted_model() {
    let (mut app, h, cmd_rx) = select_app(&["m0", "m1", "m2"]);

    app.on_key(key(KeyCode::Down), &h);
    app.on_key(key(KeyCode::Down), &h);
    assert_eq!(app.picker.as_ref().unwrap().cursor, 2, "Down moves down");
    app.on_key(key(KeyCode::Up), &h);
    assert_eq!(app.picker.as_ref().unwrap().cursor, 1, "Up moves back up");

    app.on_key(key(KeyCode::Enter), &h);
    assert!(matches!(app.mode, Mode::Idle), "Enter closes the picker");
    assert!(app.picker.is_none());
    assert!(
        matches!(cmd_rx.try_recv(), Ok(WorkerCmd::SetModel(m)) if m == "m1"),
        "Enter selects the highlighted model"
    );
}

#[test]
fn model_picker_arrows_still_work_above_the_first_row() {
    let (mut app, h, _rx) = select_app(&["m0", "m1"]);
    app.on_key(key(KeyCode::Up), &h);
    assert_eq!(app.picker.as_ref().unwrap().cursor, 0, "clamped at the top");
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT), &h);
    assert_eq!(
        app.picker.as_ref().unwrap().cursor,
        1,
        "shifted arrows work too"
    );
}

#[test]
fn model_picker_types_to_filter_without_stealing_j_k() {
    let (mut app, h, _rx) = select_app(&["deepseek-chat", "gpt-4o", "kimi-k2"]);
    for c in "kimi".chars() {
        app.on_key(key(KeyCode::Char(c)), &h);
    }
    let p = app.picker.as_ref().unwrap();
    assert_eq!(p.filter, "kimi");
    assert_eq!(p.filtered(), vec![2], "'k' filtered instead of moving up");
}

/// What's typed into a picker shows on screen, not just in the filtering.
#[test]
fn picker_shows_what_was_typed() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let (mut app, h, _rx) = select_app(&["deepseek-chat", "gpt-4o", "kimi-k2"]);
    let mut term = Terminal::new(TestBackend::new(100, 16)).unwrap();
    assert!(draw(&mut app, &mut term).contains("type to filter"));
    for c in "kim".chars() {
        app.on_key(key(KeyCode::Char(c)), &h);
    }
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("filter: kim"), "{screen}");
    assert!(!screen.contains("gpt-4o"), "{screen}");
}
