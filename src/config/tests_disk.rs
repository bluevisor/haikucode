//! Tests: config.json round trips, legacy keys, atomic writes, picoder adoption.

use super::paths::adopt_picoder_from;
use super::*;

#[test]
fn a_malformed_mcp_entry_is_skipped_but_survives_a_rewrite() {
    let mut cfg = Config::default();
    let v = serde_json::json!({
        "mcp_servers": {
            "good": {"command": "npx", "args": ["x"]},
            "bad": {"cmd": "typo"}
        }
    });
    // Mirror load_disk's per-entry logic on the parsed value.
    let m = &v["mcp_servers"];
    let mut broken = false;
    for (k, val) in m.as_object().unwrap() {
        match McpServerConfig::parse(val) {
            Some(c) => {
                cfg.mcp_servers.insert(k.clone(), c);
            }
            None => broken = true,
        }
    }
    if broken {
        cfg.extra.insert("mcp_servers".into(), m.clone());
    }
    assert_eq!(cfg.mcp_servers.len(), 1, "the good server still loads");
    let out = cfg.to_disk_json();
    assert_eq!(out["mcp_servers"], v["mcp_servers"], "the raw block round-trips verbatim");
}

#[cfg(unix)]
#[test]
fn atomic_write_keeps_the_targets_file_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("haikucode-aw-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("config.json");
    std::fs::write(&path, "{}").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    atomic_write(&path, b"{\"a\":1}").unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "rewrite must not widen permissions");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn custom_providers_round_trip_and_keep_their_model() {
    let mut c = Config::default();
    c.apply_patch(&ConfigPatch::AddProvider(CustomProvider {
        name: "gateway".into(),
        base_url: "https://gw.example/v1".into(),
        model: "m1".into(),
    }));
    c.apply_patch(&ConfigPatch::Provider {
        provider: "gateway".into(),
        base_url: "https://gw.example/v1".into(),
        model: "m1".into(),
    });
    c.apply_patch(&ConfigPatch::ApiKey("k".into()));
    // a model change while it's active is remembered in its entry
    c.model = "m2".into();
    c.sync_active_custom();
    c.apply_patch(&ConfigPatch::BaseUrl("https://gw2.example/v1".into()));
    let json = c.to_disk_json();
    let mut back = Config::default();
    back.absorb_disk(&json);
    assert_eq!(
        back.custom_provider("gateway"),
        Some(&CustomProvider { name: "gateway".into(), base_url: "https://gw2.example/v1".into(), model: "m2".into() })
    );
    assert_eq!(back.api_keys.get("gateway").map(String::as_str), Some("k"));
    let names: Vec<String> = back.provider_choices().into_iter().map(|(n, _, _)| n).collect();
    assert_eq!(names.last().map(String::as_str), Some("gateway"), "presets first, then custom");
    // removing forgets its key too
    back.apply_patch(&ConfigPatch::RemoveProvider("gateway".into()));
    assert!(back.custom_provider("gateway").is_none() && !back.api_keys.contains_key("gateway"));
}

#[test]
fn a_wizard_custom_provider_joins_the_list_on_load() {
    // Written by the setup wizard before custom providers were a list.
    let v = serde_json::json!({
        "provider": "litellm",
        "base_url": "https://llm.example/v1",
        "model": "gemini-x",
        "api_keys": {"litellm": "k", "deepseek": "d"}
    });
    let mut c = Config::default();
    c.absorb_disk(&v);
    assert_eq!(c.custom_provider("litellm").map(|p| p.model.as_str()), Some("gemini-x"));
    // a preset never joins, and malformed entries are skipped
    let v = serde_json::json!({
        "provider": "deepseek",
        "custom_providers": [{"name": "openai", "base_url": "https://x", "model": "m"}, {"name": "ok", "base_url": "https://y", "model": "m"}, 3]
    });
    let mut c = Config::default();
    c.absorb_disk(&v);
    let names: Vec<&str> = c.custom_providers.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["ok"]);
}

#[test]
fn price_currency_is_normalized_and_persisted_only_when_set() {
    let mut c = Config::default();
    assert_eq!(
        c.price_currency, "USD",
        "prices are quoted in USD by default"
    );
    assert!(
        c.to_disk_json().get("price_currency").is_none(),
        "the default stays out of the file, like the other price fields"
    );

    c.apply_patch(&ConfigPatch::PriceCurrency("cny".into()));
    assert_eq!(c.price_currency, "CNY", "case is normalized on the way in");
    assert_eq!(c.to_disk_json()["price_currency"], "CNY");

    // A blank entry falls back to the documented default rather than
    // leaving the status line with no unit at all.
    c.apply_patch(&ConfigPatch::PriceCurrency("   ".into()));
    assert_eq!(c.price_currency, "USD");
    assert!(c.to_disk_json().get("price_currency").is_none());
}

#[test]
fn a_hand_edited_price_currency_is_read_back() {
    let v: serde_json::Value = serde_json::from_str(r#"{"price_currency": "cny"}"#).unwrap();
    let code = v.get("price_currency").and_then(|x| x.as_str()).unwrap();
    assert_eq!(crate::money::Currency::parse(code).symbol, "¥");
}

/// The old `thinking: true` did something only on DeepSeek/GLM. Elsewhere
/// (here: a local Qwen behind Splash) it must stay a no-op, not become a
/// request that switches reasoning on.
#[test]
fn legacy_thinking_flag_keeps_its_old_reach() {
    let load = |v: serde_json::Value| {
        let mut c = Config::default();
        c.absorb_disk(&v);
        c.reasoning
    };
    assert_eq!(load(serde_json::json!({"provider": "deepseek", "thinking": true})), "medium");
    assert_eq!(load(serde_json::json!({"provider": "zai", "thinking": true})), "medium");
    assert_eq!(load(serde_json::json!({"provider": "splash", "thinking": true})), default_reasoning());
    // An explicit setting always wins.
    assert_eq!(load(serde_json::json!({"provider": "splash", "thinking": true, "reasoning": "high"})), "high");
}

#[test]
fn statusline_round_trips_and_stays_out_of_the_file_when_default() {
    let mut c = Config::default();
    assert!(c.to_disk_json().get("statusline").is_none());
    c.statusline = vec!["git".into(), "ctx".into()];
    let json = c.to_disk_json();
    assert_eq!(json["statusline"], serde_json::json!(["git", "ctx"]));
    let mut back = Config::default();
    back.absorb_disk(&json);
    assert_eq!(back.statusline, ["git", "ctx"]);
}

#[test]
fn atomic_write_replaces_and_leaves_no_temp() {
    let dir = std::env::temp_dir().join(format!("haikucode_atomic_test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.json");
    std::fs::write(&path, b"old contents").unwrap();
    atomic_write(&path, b"new contents").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new contents");
    // The per-process temp file must not survive a successful write.
    let leftover = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().contains(".tmp."));
    assert!(!leftover, "atomic_write left a temp file behind");
    // Writing to a brand-new path works too.
    let fresh = dir.join("fresh.json");
    atomic_write(&fresh, b"{}").unwrap();
    assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "{}");
    std::fs::remove_dir_all(&dir).ok();
}

/// picoder's directory is copied over once, without its `run/` sockets,
/// and never over an existing Haiku Code setup.
#[test]
fn picoder_settings_are_adopted_once() {
    let root = std::env::temp_dir().join(format!("hc-adopt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (old, new) = (root.join("picoder"), root.join("haikucode"));
    std::fs::create_dir_all(old.join("sessions/abc")).unwrap();
    std::fs::create_dir_all(old.join("run")).unwrap();
    std::fs::write(old.join("config.json"), "{}").unwrap();
    std::fs::write(old.join("sessions/abc/1.json"), "[]").unwrap();
    std::fs::write(old.join("run/sock"), "").unwrap();
    assert!(adopt_picoder_from(&old, &new).unwrap().starts_with("imported"));
    assert!(new.join("config.json").is_file());
    assert!(new.join("sessions/abc/1.json").is_file());
    assert!(!new.join("run").exists());
    assert!(adopt_picoder_from(&old, &new).is_none(), "second run copies nothing");
    let _ = std::fs::remove_dir_all(&root);
}
