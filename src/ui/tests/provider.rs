use super::*;

#[test]
fn provider_command_adds_switches_and_removes_custom_providers() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, rx) = test_handles();
    let run = |app: &mut App, line: &str| {
        app.mode = Mode::Idle;
        app.input = line.to_string();
        app.cursor = app.input.chars().count();
        app.submit(&h);
    };
    let patches = |rx: &std::sync::mpsc::Receiver<WorkerCmd>| -> Vec<ConfigPatch> {
        rx.try_iter()
            .filter_map(|c| match c {
                WorkerCmd::Patch(p) => Some(p),
                _ => None,
            })
            .collect()
    };

    run(&mut app, "/provider add box https://llm.example/v1/ some-model");
    let sent = patches(&rx);
    assert!(matches!(&sent[0], ConfigPatch::AddProvider(c)
        if c.name == "box" && c.base_url == "https://llm.example/v1" && c.model == "some-model"));
    assert!(matches!(&sent[1], ConfigPatch::Provider { provider, .. } if provider == "box"));
    assert_eq!(app.settings.provider, "box");

    // The /config provider row cycles through it too: box is last, so
    // the next one wraps to the first preset.
    app.activate_setting(0, 1, &h);
    assert_eq!(app.settings.provider, crate::config::PROVIDERS[0].0);
    app.settings.api_keys.insert("box".into(), "k".into()); // set up, so /provider box just switches
    run(&mut app, "/provider box");
    assert_eq!((app.settings.provider.as_str(), app.settings.model.as_str()), ("box", "some-model"));
    let _ = patches(&rx);

    // Presets can't be added over or removed; bad input is refused.
    run(&mut app, "/provider add openai https://x/v1 m");
    run(&mut app, "/provider remove deepseek");
    run(&mut app, "/provider add bad name! https://x/v1");
    run(&mut app, "/provider add box2 ftp://x m");
    assert!(patches(&rx).is_empty(), "no patch for refused input");

    // Removing the active custom provider falls back to the first preset.
    run(&mut app, "/provider remove box");
    let sent = patches(&rx);
    assert!(matches!(&sent[0], ConfigPatch::Provider { provider, .. } if provider == crate::config::PROVIDERS[0].0));
    assert!(matches!(&sent[1], ConfigPatch::RemoveProvider(n) if n == "box"));
    assert!(app.settings.custom_provider("box").is_none());
    run(&mut app, "/provider box");
    assert!(app.transcript.iter().any(|l| l.kind == crate::ui::types::Kind::ErrorK && l.text.contains("unknown provider")));
}

#[test]
fn editing_a_server_prefills_values_and_keeps_the_key_and_active_provider() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, rx) = test_handles();
    app.settings.custom_providers.push(crate::config::CustomProvider {
        name: "local".into(), base_url: "http://localhost:1234/v1".into(), model: "saved-model".into(),
    });
    app.settings.api_keys.insert("local".into(), "saved-secret".into());
    let active = app.settings.provider.clone();
    app.provider_command(Some("edit local"), &h);
    assert_eq!(app.wiz_input, "http://localhost:1234/v1");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &h);
    // Same address: the saved key is offered (by its tail, never in full) and highlighted.
    let p = app.picker.as_ref().expect("the sign-in step");
    assert!(p.items[p.cursor].contains("use the saved key"), "{:?}", p.items);
    assert!(!p.items.iter().any(|i| i.contains("saved-secret")));
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &h);
    assert_eq!(app.settings.api_keys["local"], "saved-secret");
    // An inactive server is saved without switching to it.
    assert_eq!(app.settings.provider, active);
    assert!(app.wizard.is_none());
    assert_eq!(sent_kinds(&rx), ["add local http://localhost:1234/v1 [saved-model]", "auth local api"]);
}

#[test]
fn server_edits_can_go_back_and_cancel_without_saving() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, rx) = test_handles();
    app.provider_command(Some("add"), &h);
    app.on_paste("local".into());
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &h);
    app.on_paste("http://localhost:1234/v1".into());
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &h);
    pick_row(&mut app, &h, "API key — paste");
    app.on_paste("unsaved-secret".into());
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
    let screen = draw(&mut app, &mut term);
    assert!(!screen.contains("unsaved-secret"), "the key is masked");
    // Shift+Tab goes back a step like Esc: to the sign-in step, then the address.
    app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT), &h);
    assert!(app.picker.as_ref().is_some_and(|p| matches!(p.action, crate::ui::types::PickAction::Auth)));
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &h);
    for _ in 0..4 {
        if app.wizard.is_none() { break; }
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &h);
    }
    assert!(app.wizard.is_none());
    assert!(app.wiz_input.is_empty());
    assert!(rx.try_iter().next().is_none(), "nothing saved");
    assert!(app.settings.custom_provider("local").is_none());
}

#[test]
fn editing_server_addresses_never_reuses_a_saved_key_by_default() {
    for (row, typed, expected) in [("No key", "", ""), ("API key — paste", "replacement", "replacement")] {
        let mut app = App::new(test_ui_config(), Vec::new());
        let (h, rx) = test_handles();
        let cp = crate::config::CustomProvider {
            name: "local".into(), base_url: "http://localhost:1234/v1".into(), model: "m".into(),
        };
        app.settings.custom_providers.push(cp);
        app.settings.api_keys.insert("local".into(), "old-secret".into());
        app.provider_command(Some("edit local"), &h);
        app.wizard_answer(ProviderStep::BaseUrl, "https://new.example/v1".into(), &h);
        let p = app.picker.as_ref().expect("the sign-in step");
        assert!(!p.items.iter().any(|i| i.contains("saved key")), "{:?}", p.items);
        assert!(p.items[p.cursor].contains("No key"));
        assert!(app.wizard.as_ref().and_then(|d| d.error.as_deref()).is_some_and(|e| e.contains("address changed")));
        pick_row(&mut app, &h, row);
        if !typed.is_empty() {
            app.on_paste(typed.into());
            app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &h);
        }
        assert_eq!(app.settings.api_keys.get("local").map(String::as_str).unwrap_or(""), expected);
        assert!(rx.try_iter().any(|cmd| matches!(cmd,
            WorkerCmd::Patch(ConfigPatch::ProviderKey { key, .. }) if key == expected)));
    }
}

#[test]
fn wizard_supports_in_place_unicode_edits_and_paste() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, _rx) = test_handles();
    app.wizard = Some(ProviderDraft { adding: true, base_url: "ab語cd".into(), ..Default::default() });
    app.ask(ProviderStep::BaseUrl);
    app.on_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE), &h);
    app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &h);
    app.on_paste("X".into());
    assert_eq!(app.wiz_input, "aXb語cd");
    app.on_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE), &h);
    app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &h);
    app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE), &h);
    assert_eq!(app.wiz_input, "aXcd");
    app.on_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), &h);
    app.on_paste("Z".into());
    assert_eq!(app.wiz_input, "aXcdZ");
}

#[test]
fn custom_servers_without_keys_switch_without_an_unnecessary_key_prompt() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, rx) = test_handles();
    app.settings.custom_providers.push(crate::config::CustomProvider {
        name: "local".into(), base_url: "http://localhost:1234/v1".into(), model: "m".into(),
    });
    app.open_provider_picker();
    let p = app.picker.as_mut().unwrap();
    p.cursor = p.items.iter().position(|i| i.starts_with("local ")).unwrap();
    app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL), &h);
    assert!(matches!(app.mode, Mode::ProviderInput { step: ProviderStep::BaseUrl }));
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &h);
    assert!(app.wizard.is_none(), "Esc on an edit's first step cancels");
    app.provider_picked("local", &h);
    // No key saved: No key is highlighted, so one Enter switches.
    let p = app.picker.as_ref().expect("the sign-in step");
    assert!(p.items[p.cursor].contains("No key"), "{:?}", p.items);
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &h);
    assert_eq!(app.settings.provider, "local");
    assert!(rx.try_iter().any(|cmd| matches!(cmd, WorkerCmd::ListModels)));
}

#[test]
fn wizard_hints_and_the_cursor_remain_visible_in_narrow_terminals() {
    let mut app = App::new(test_ui_config(), Vec::new());
    app.wizard = Some(ProviderDraft { adding: true, base_url: format!("https://gateway.example/{}/v1", "long-path/".repeat(8)), ..Default::default() });
    app.ask(ProviderStep::BaseUrl);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(46, 20)).unwrap();
    let screen = draw(&mut app, &mut term);
    assert!(screen.contains("Base URL"), "{screen}");
    assert!(screen.contains("Esc goes back"), "{screen}");
    let buf = term.backend().buffer();
    assert!(buf.content().iter().any(|c| c.modifier.contains(ratatui::style::Modifier::REVERSED)), "cursor must remain visible: {screen}");
}

#[test]
fn clearing_a_saved_key_and_editing_the_active_server_updates_live_settings() {
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, rx) = test_handles();
    let cp = crate::config::CustomProvider { name: "local".into(), base_url: "http://localhost:1234/v1".into(), model: "old-model".into() };
    app.settings.custom_providers.push(cp.clone());
    app.settings.provider = cp.name;
    app.settings.base_url = cp.base_url;
    app.settings.model = cp.model;
    app.settings.api_keys.insert("local".into(), "old-secret".into());
    app.settings.resolve_key();
    app.provider_command(Some("edit"), &h);
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &h);
    // No key drops the saved one; the active server switches live.
    pick_row(&mut app, &h, "No key");
    assert_eq!(app.settings.provider, "local");
    assert_eq!(app.settings.api_key, "");
    assert!(app.settings.api_keys.get("local").is_none_or(|k| k.is_empty()));
    app.handle_event(UiEvent::ModelList(vec!["old-model".into(), "new-model".into()]), &h);
    pick_row(&mut app, &h, "new-model");
    app.handle_event(UiEvent::ModelChanged("new-model".into()), &h); // the worker's ack
    assert_eq!(app.settings.model, "new-model");
    assert_eq!(app.model_info, "new-model");
    let sent = sent_kinds(&rx);
    assert!(sent.contains(&"key local ".to_string()), "{sent:?}");
    assert!(sent.contains(&"model new-model".to_string()), "{sent:?}");
}

#[test]
fn provider_wizard_adds_a_provider_step_by_step() {
    use crate::ui::types::{PickAction, ProviderStep};
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, rx) = test_handles();
    let typed = |app: &mut App, text: &str| {
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL), &h);
        for c in text.chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &h);
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &h);
    };
    app.mode = Mode::Idle;
    app.input = "/provider".into();
    app.cursor = app.input.chars().count();
    app.submit(&h);
    assert!(matches!(app.mode, Mode::Select));
    let p = app.picker.as_mut().expect("provider picker");
    assert!(matches!(p.action, PickAction::Provider));
    assert!(p.hint.as_deref().is_some_and(|t| t.contains("step 1 of 3")));
    assert_eq!(p.items.last().map(String::as_str), Some(App::ADD_PROVIDER));
    p.cursor = p.items.len() - 1;
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &h);
    assert!(matches!(app.mode, Mode::ProviderInput { step: ProviderStep::Name }));

    // Each step refuses a bad answer and stays put.
    typed(&mut app, "bad name");
    assert!(matches!(app.mode, Mode::ProviderInput { step: ProviderStep::Name }));
    assert!(app.wizard.as_ref().unwrap().error.is_some());
    typed(&mut app, "box");
    assert!(matches!(app.mode, Mode::ProviderInput { step: ProviderStep::BaseUrl }));
    typed(&mut app, "ftp://x");
    assert!(matches!(app.mode, Mode::ProviderInput { step: ProviderStep::BaseUrl }));
    typed(&mut app, "https://llm.example/v1/");

    // Sign-in: a custom server takes a key or none.
    let p = app.picker.as_ref().expect("sign-in picker");
    assert!(matches!(p.action, PickAction::Auth));
    assert!(p.items.iter().any(|i| i.starts_with("No key")));
    pick_row(&mut app, &h, "API key — paste");
    assert!(matches!(app.mode, Mode::ProviderInput { step: ProviderStep::ApiKey }));
    typed(&mut app, "two words");
    assert!(app.wizard.as_ref().unwrap().error.is_some(), "a key with spaces is refused");
    typed(&mut app, "Bearer secret");

    assert_eq!(
        sent_kinds(&rx),
        ["add box https://llm.example/v1 []", "key box secret", "auth box api", "switch box", "models"]
    );
    assert!(app.wizard.as_ref().is_some_and(|d| d.waiting_models));

    // The model step: the server's list, and a name it doesn't list
    // is taken as typed.
    app.handle_event(UiEvent::ModelList(vec!["alpha".into(), "beta".into()]), &h);
    let p = app.picker.as_ref().expect("model picker");
    assert!(p.title.contains("Choose a model") && p.hint.as_deref().is_some_and(|t| t.contains("step 4 of 4")));
    for c in "gamma-7b".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &h);
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &h);
    assert_eq!(sent_kinds(&rx), ["model gamma-7b"]);
    assert!(app.wizard.is_none());
    assert!(matches!(app.mode, Mode::Idle));
    assert!(app.transcript.iter().any(|l| l.text.contains("box · API key · gamma-7b")));
}

#[test]
fn a_saved_key_is_kept_with_enter_and_esc_goes_back_a_step() {
    use crate::ui::types::{PickAction, ProviderStep};
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, rx) = test_handles();
    app.settings.api_keys.insert("deepseek".into(), "sk-abcd1234".into());
    app.open_provider_picker();
    let p = app.picker.as_ref().unwrap();
    assert!(p.items.iter().any(|i| i.starts_with("deepseek") && i.contains("API key")), "set up shows as ready");
    assert!(p.items.iter().any(|i| i.starts_with("groq") && i.contains("not set up")));
    pick_row(&mut app, &h, "deepseek");

    // The saved key is the highlighted row: Enter keeps it.
    let p = app.picker.as_ref().unwrap();
    assert!(matches!(p.action, PickAction::Auth));
    assert!(p.items[p.cursor].contains("saved key …1234"));
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &h);
    assert_eq!(sent_kinds(&rx), ["auth deepseek api", "switch deepseek", "models"]);

    // Esc on the model list keeps the model in use and ends the setup.
    app.handle_event(UiEvent::ModelList(vec!["deepseek-flash".into()]), &h);
    app.do_esc(&h);
    assert!(app.wizard.is_none() && matches!(app.mode, Mode::Idle));

    // Esc walks back: key entry → sign-in → provider list.
    app.open_provider_picker();
    pick_row(&mut app, &h, "groq");
    pick_row(&mut app, &h, "API key — paste");
    assert!(matches!(app.mode, Mode::ProviderInput { step: ProviderStep::ApiKey }));
    app.do_esc(&h);
    assert!(app.picker.as_ref().is_some_and(|p| matches!(p.action, PickAction::Auth)));
    app.do_esc(&h);
    assert!(app.picker.as_ref().is_some_and(|p| matches!(p.action, PickAction::Provider)));
    app.do_esc(&h);
    assert!(matches!(app.mode, Mode::Idle));
    assert!(rx.try_iter().next().is_none(), "backing out sends nothing");
}

#[test]
fn chatgpt_sign_in_then_model_and_a_refused_key_goes_back() {
    use crate::ui::types::{PickAction, ProviderStep};
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, rx) = test_handles();
    app.open_provider_picker();
    pick_row(&mut app, &h, "openai");
    let p = app.picker.as_ref().unwrap();
    assert!(p.items.iter().any(|i| i.contains("API key")));
    pick_row(&mut app, &h, "ChatGPT subscription");
    assert_eq!(sent_kinds(&rx), ["login openai"]);
    assert!(matches!(app.mode, Mode::Busy), "waits for the browser");

    // Esc / a failed sign-in comes back to the sign-in step with why.
    app.handle_event(UiEvent::SignInFailed { provider: "openai".into(), error: "sign-in cancelled".into() }, &h);
    assert!(app.picker.as_ref().is_some_and(|p| matches!(p.action, PickAction::Auth)));
    assert!(app.wizard.as_ref().and_then(|d| d.error.as_deref()).is_some_and(|e| e.contains("cancelled")));
    pick_row(&mut app, &h, "ChatGPT subscription");
    let _ = sent_kinds(&rx);
    let token = crate::config::OAuthToken { access_token: "tok".into(), ..Default::default() };
    app.handle_event(UiEvent::SignedIn { provider: "openai".into(), token }, &h);
    assert_eq!(sent_kinds(&rx), ["auth openai sub", "switch openai", "models"]);
    assert!(app.settings.uses_sub() && app.settings.chatgpt_sub());
    app.handle_event(UiEvent::ModelList(vec!["gpt-a".into(), "gpt-b".into()]), &h);
    pick_row(&mut app, &h, "gpt-b");
    assert_eq!(sent_kinds(&rx), ["model gpt-b"]);
    assert!(app.transcript.iter().any(|l| l.text.contains("openai · ChatGPT sign-in · gpt-b")));

    // Signed in, the sign-in step offers to keep it, re-sign or sign out.
    app.open_provider_picker();
    pick_row(&mut app, &h, "openai");
    let p = app.picker.as_ref().unwrap();
    assert!(p.items[p.cursor].contains("signed in"), "the sign-in in use is highlighted");
    pick_row(&mut app, &h, "Sign out");
    assert_eq!(sent_kinds(&rx), ["signout openai"]);
    assert!(!app.settings.signed_in("openai") && !app.settings.uses_sub());

    // A key the server refuses goes back to the sign-in step.
    pick_row(&mut app, &h, "API key — paste");
    for c in "sk-bad".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &h);
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &h);
    let _ = sent_kinds(&rx);
    app.handle_event(UiEvent::ModelListFailed("HTTP 401: invalid api key".into()), &h);
    assert!(app.picker.as_ref().is_some_and(|p| matches!(p.action, PickAction::Auth)));
    assert!(app.wizard.as_ref().and_then(|d| d.error.as_deref()).is_some_and(|e| e.contains("refused the key")));

    // Any other failure asks for the model id by hand.
    pick_row(&mut app, &h, "use the saved key");
    let _ = sent_kinds(&rx);
    app.handle_event(UiEvent::ModelListFailed("network error: timed out".into()), &h);
    assert!(matches!(app.mode, Mode::ProviderInput { step: ProviderStep::Model }));
    for c in "gpt-c".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &h);
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &h);
    assert_eq!(sent_kinds(&rx), ["model gpt-c"]);
    assert!(app.wizard.is_none());
}

#[test]
fn sign_in_is_per_provider_and_offered_only_where_possible() {
    use crate::config::{auth_methods, AuthMethod};
    assert_eq!(auth_methods("openai"), [AuthMethod::Api, AuthMethod::Sub]);
    assert_eq!(auth_methods("anthropic"), [AuthMethod::Api]);
    assert_eq!(auth_methods("zai"), [AuthMethod::Api, AuthMethod::Plan]);
    assert_eq!(auth_methods("my-box"), [AuthMethod::Api, AuthMethod::None]);
    let mut c = crate::config::Config::default();
    c.oauth.insert("openai".into(), crate::config::OAuthToken { access_token: "t".into(), ..Default::default() });
    c.apply_patch(&ConfigPatch::ProviderAuth { provider: "openai".into(), mode: "sub".into() });
    c.apply_patch(&ConfigPatch::ProviderAuth { provider: "anthropic".into(), mode: "sub".into() });
    assert_eq!(c.auth_for("anthropic"), AuthMethod::Api, "a method the provider lacks is ignored");
    c.apply_patch(&ConfigPatch::Provider { provider: "openai".into(), base_url: "https://api.openai.com/v1".into(), model: "m".into() });
    assert!(c.chatgpt_sub() && c.endpoint_base() == crate::config::CHATGPT_CODEX_URL && c.bearer() == "t");
    c.apply_patch(&ConfigPatch::Provider { provider: "deepseek".into(), base_url: "https://api.deepseek.com".into(), model: "m".into() });
    assert!(!c.uses_sub(), "the subscription doesn't follow you to another provider");
    c.apply_patch(&ConfigPatch::ProviderAuth { provider: "zai".into(), mode: "plan".into() });
    c.apply_patch(&ConfigPatch::Provider { provider: "zai".into(), base_url: "https://api.z.ai/api/paas/v4".into(), model: "m".into() });
    assert_eq!(c.endpoint_base(), "https://api.z.ai/api/coding/paas/v4");
}

#[test]
fn re_running_sign_in_keeps_the_active_presets_proxy_url() {
    let (mut app, _h) = scrollable_app();
    app.settings.provider = "openai".into();
    app.settings.base_url = "https://proxy.example/v1".into();
    app.settings.model = "gpt-x".into();
    app.setup_provider("openai");
    let d = app.wizard.as_ref().expect("the sign-in step");
    assert_eq!((d.base_url.as_str(), d.model.as_str()), ("https://proxy.example/v1", "gpt-x"));
}

#[test]
fn a_key_pasted_on_the_sign_in_list_opens_the_key_prompt_with_it() {
    use crate::ui::types::{PickAction, ProviderStep};
    let mut app = App::new(test_ui_config(), Vec::new());
    let (h, rx) = test_handles();
    app.open_provider_picker();
    pick_row(&mut app, &h, "groq");
    assert!(app.picker.as_ref().is_some_and(|p| matches!(p.action, PickAction::Auth)));

    // Pasted on the list (the row says "paste a key"): not a filter.
    app.on_paste("  gsk_abcd1234\n".into());
    assert!(matches!(app.mode, Mode::ProviderInput { step: ProviderStep::ApiKey }));
    assert_eq!(app.wiz_input, "gsk_abcd1234");
    assert!(rx.try_iter().next().is_none(), "a paste never submits");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &h);
    assert_eq!(sent_kinds(&rx), ["key groq gsk_abcd1234", "auth groq api", "switch groq", "models"]);

    // A blank paste stays on the list.
    app.open_provider_picker();
    pick_row(&mut app, &h, "groq");
    app.on_paste("   ".into());
    assert!(app.picker.as_ref().is_some_and(|p| matches!(p.action, PickAction::Auth)), "blank paste stays on the list");
}
