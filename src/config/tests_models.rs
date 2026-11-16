//! Tests: providers, patches, price lists, per-model settings, context windows.

use super::*;

#[test]
fn server_addresses_reject_mistakes_before_saving_credentials() {
    for valid in ["http://localhost:1234/v1", "http://127.0.0.1:11434/v1", "https://gateway.example/api/v1", "http://[::1]:8080/v1"] {
        assert_eq!(super::normalize_provider_url(&format!(" {valid}/ ")).unwrap(), valid);
    }
    for invalid in ["", "localhost:1234", "https://", "https:host/v1", "http:/host/v1", "ftp://host/v1", "https://host:bad/v1",
        "https://host/v1 with space", "https://host/v1?key=secret", "https://host/v1#fragment",
        "https://user:secret@host/v1", "https://host/v1/chat/completions", "https://host/v1/models/"] {
        assert!(super::normalize_provider_url(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn patches_map_to_fields() {
    let mut c = Config::default();
    c.apply_patch(&ConfigPatch::Provider {
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        model: "gpt-4o-mini".into(),
    });
    assert_eq!((c.provider.as_str(), c.model.as_str()), ("openai", "gpt-4o-mini"));
    c.apply_patch(&ConfigPatch::Reasoning("High".into()));
    c.apply_patch(&ConfigPatch::Permission("plan".into()));
    c.apply_patch(&ConfigPatch::AutoCommit(false));
    c.apply_patch(&ConfigPatch::ContextWindow(64000));
    c.apply_patch(&ConfigPatch::MaxToolCalls(42));
    assert_eq!(c.reasoning, "high");
    assert_eq!(c.permission, "plan");
    assert!(!c.auto_commit);
    // A window set from the panel belongs to the model it's set on.
    assert_eq!(c.model_context_window(), Some(64000));
    assert_eq!(c.max_tool_calls, 42);
    // A key set through the panel always counts as a disk key.
    c.key_from_env = true;
    c.apply_patch(&ConfigPatch::ApiKey("sk-x".into()));
    assert!(!c.key_from_env);
    // Zero context window is clamped, not propagated.
    c.apply_patch(&ConfigPatch::ContextWindow(0));
    assert_eq!(c.model_context_window(), Some(1));
    // The pin is the model's, so it survives a reload (written under `models`).
    assert_eq!(c.to_disk_json()["models"][&c.model]["context_window"], 1);
}

/// The cost counter follows the provider: switching presets takes their
/// built-in price lists with them; a custom server without a list has no
/// known cost (and the top-level mirror zeros out).
#[test]
fn the_price_list_follows_the_active_provider() {
    let mut c = Config::default();
    let ds = c.price_list().unwrap();
    assert!((ds.price_in - 0.15).abs() < 1e-9 && (ds.price_out - 0.6).abs() < 1e-9);
    assert_eq!(ds.price_currency, "USD");

    c.apply_patch(&ConfigPatch::Provider {
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        model: "gpt-4o-mini".into(),
    });
    let o = c.price_list().unwrap();
    assert!((o.price_in - 0.15).abs() < 1e-9 && (o.price_out - 0.6).abs() < 1e-9);
    assert!((o.price_cached() - 0.075).abs() < 1e-9, "OpenAI's cache-read price");

    c.apply_patch(&ConfigPatch::Provider {
        provider: "anthropic".into(),
        base_url: "https://api.anthropic.com/v1".into(),
        model: "claude-opus-5-5".into(),
    });
    let a = c.price_list().unwrap();
    assert!((a.price_in - 15.0).abs() < 1e-9 && (a.price_out - 75.0).abs() < 1e-9);
    // The top-level fields mirror the active provider's list.
    assert!((c.price_in - 15.0).abs() < 1e-9 && (c.price_out - 75.0).abs() < 1e-9);

    c.apply_patch(&ConfigPatch::Provider {
        provider: "mylocal".into(),
        base_url: "http://localhost:11434/v1".into(),
        model: "qwen3".into(),
    });
    assert!(c.price_list().is_none(), "a custom server has no known price");
    assert_eq!(c.price_in, 0.0, "the mirror zeros out");

    c.apply_patch(&ConfigPatch::Provider {
        provider: "anthropic".into(),
        base_url: "https://api.anthropic.com/v1".into(),
        model: "claude-opus-5-5".into(),
    });
    assert!((c.price_list().unwrap().price_in - 15.0).abs() < 1e-9, "switching back keeps the table's list");
}

/// A user-set price list wins over the built-in table and survives
/// switching providers and back.
#[test]
fn a_user_price_list_overrides_the_table_and_survives_a_round_trip() {
    let mut c = Config::default();
    c.prices.insert(
        "deepseek".into(),
        PriceList {
            price_in: 1.0,
            price_out: 4.0,
            price_cached_in: None,
            price_currency: "CNY".into(),
        },
    );
    c.sync_prices();
    assert_eq!(c.price_list().unwrap().price_currency, "CNY");
    assert_eq!(c.price_currency, "CNY", "the mirror follows the user's list");

    c.apply_patch(&ConfigPatch::Provider {
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        model: "gpt-4o-mini".into(),
    });
    assert_eq!(c.price_list().unwrap().price_currency, "USD", "openai gets its own list");

    c.apply_patch(&ConfigPatch::Provider {
        provider: "deepseek".into(),
        base_url: "https://api.deepseek.com".into(),
        model: "deepseek-flash".into(),
    });
    let back = c.price_list().unwrap();
    assert!((back.price_in - 1.0).abs() < 1e-9 && back.price_currency == "CNY", "the CNY list comes back");
}

/// A pre-per-provider file carries one top-level price list; it belongs
/// to the provider that was active when it was written, and the built-in
/// cache-hit price fills in where the file has none.
#[test]
fn a_legacy_top_level_price_list_folds_into_the_active_provider() {
    let v = serde_json::json!({
        "provider": "deepseek",
        "price_in": 1.0,
        "price_out": 4.0,
        "price_currency": "cny",
    });
    let mut c = Config::default();
    c.absorb_disk(&v);
    c.sync_prices();
    let list = c.price_list().unwrap();
    assert!((list.price_in - 1.0).abs() < 1e-9 && list.price_currency == "CNY");
    assert!((list.price_cached() - DEEPSEEK_CACHE_HIT_PRICE).abs() < 1e-9, "the built-in cache-hit price carries over");
    // Switching to another preset and back keeps the folded list.
    c.apply_patch(&ConfigPatch::Provider {
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        model: "gpt-4o-mini".into(),
    });
    c.apply_patch(&ConfigPatch::Provider {
        provider: "deepseek".into(),
        base_url: "https://api.deepseek.com".into(),
        model: "deepseek-flash".into(),
    });
    assert!((c.price_list().unwrap().price_in - 1.0).abs() < 1e-9);
}

/// A custom server without a price list stays hidden across save/reload:
/// the zeroed top-level mirror must not be written as a user price list.
#[test]
fn a_custom_provider_without_a_list_keeps_its_cost_hidden() {
    let mut c = Config::default();
    c.custom_providers.push(CustomProvider {
        name: "mylocal".into(),
        base_url: "http://localhost:11434/v1".into(),
        model: "qwen3".into(),
    });
    c.apply_patch(&ConfigPatch::Provider {
        provider: "mylocal".into(),
        base_url: "http://localhost:11434/v1".into(),
        model: "qwen3".into(),
    });
    assert!(c.price_list().is_none());
    let json = c.to_disk_json();
    assert!(json.get("price_in").is_none() && json.get("price_currency").is_none(), "{json}");

    let mut back = Config::default();
    back.absorb_disk(&json);
    back.sync_prices();
    assert!(back.price_list().is_none(), "still hidden after a round trip");
    assert_eq!(back.price_in, 0.0);
}

#[test]
fn deepseek_flash_takes_images_but_pro_does_not() {
    let mut c = Config::default();
    assert_eq!(c.model, "deepseek-flash");
    assert!(c.supports_images());
    c.model = "deepseek-v4-pro".into();
    assert!(!c.supports_images());
    c.provider = "zai".into();
    c.model = "glm-5.2".into();
    assert!(!c.supports_images());
}

#[test]
fn temperature_is_only_sent_when_configured_or_for_deepseek() {
    let mut c = Config::default();
    assert_eq!(c.provider, "deepseek");
    assert_eq!(c.temperature(), Some(0.2), "DeepSeek keeps its old 0.2");
    c.provider = "splash".into();
    assert_eq!(c.temperature(), None, "others get the model's own sampling");
    c.temperature = Some(0.7);
    assert_eq!(c.temperature(), Some(0.7));
}

#[test]
fn per_model_context_windows() {
    let v = serde_json::json!({
        "provider": "deepseek",
        "model": "Gemma4-26B-A4B-QAT",
        "context_window": 64000,
        "models": {
            "Gemma4-26B-A4B-QAT": {"context_window": 131072},
            "bad": {"context_window": "x"},
            "zero": {"context_window": 0}
        },
        // legacy map: folded in, never overriding a `models` value
        "model_context_windows": {"gemma4-coding-Q8_0": 8192, "Gemma4-26B-A4B-QAT": 1}
    });
    let mut c = Config::default();
    c.absorb_disk(&v);
    // the model's entry beats the global window; bad/zero entries are dropped
    assert_eq!(c.model_context_window(), Some(131072));
    assert_eq!(c.effective_context_window(), 131072);
    assert_eq!(c.models.len(), 2, "{:?}", c.models);
    // case-insensitive match
    c.model = "GEMMA4-CODING-q8_0".into();
    assert_eq!(c.effective_context_window(), 8192);
    // no entry: the global window
    c.model = "deepseek-flash".into();
    assert_eq!(c.model_context_window(), None);
    assert_eq!(c.effective_context_window(), 64000);
    // /config pins the window on the active model, never globally, so
    // the next /model doesn't inherit it...
    c.apply_patch(&ConfigPatch::ContextWindow(100_000));
    assert_eq!((c.context_window, c.effective_context_window()), (64000, 100_000));
    assert_eq!(c.models["deepseek-flash"].context_window, Some(100_000));
    // ...updating an existing entry under its stored spelling
    c.model = "gemma4-26b-a4b-qat".into();
    c.apply_patch(&ConfigPatch::ContextWindow(65536));
    assert_eq!(c.models["Gemma4-26B-A4B-QAT"].context_window, Some(65536));
    assert_eq!(c.context_window, 64000);
    // round-trips through the file as `models`; the legacy key isn't rewritten
    let json = c.to_disk_json();
    assert_eq!(json["models"]["Gemma4-26B-A4B-QAT"]["context_window"], 65536);
    assert!(json.get("model_context_windows").is_none());
    let mut back = Config::default();
    back.absorb_disk(&json);
    assert_eq!(back.models, c.models);
    assert!(Config::default().to_disk_json().get("models").is_none());
}

#[test]
fn presets_remember_their_model_and_proxy_url() {
    let mut c = Config::default();
    let (openai_url, openai_model) = PROVIDERS.iter().find(|(n, ..)| *n == "openai").map(|(_, b, m)| (*b, *m)).unwrap();
    c.apply_patch(&ConfigPatch::Provider { provider: "openai".into(), base_url: openai_url.into(), model: openai_model.into() });
    c.apply_patch(&ConfigPatch::BaseUrl("https://proxy.example/v1".into()));
    c.model = "gpt-x".into();
    c.apply_patch(&ConfigPatch::Provider { provider: "deepseek".into(), base_url: "https://api.deepseek.com".into(), model: "deepseek-flash".into() });
    let back = c.provider_choices().into_iter().find(|(n, ..)| n == "openai").unwrap();
    assert_eq!((back.1.as_str(), back.2.as_str()), ("https://proxy.example/v1", "gpt-x"));
    // the preset's own URL isn't stored, so a later preset change applies
    assert_eq!(c.presets["deepseek"].base_url, None);
    // and it survives a reload
    let mut r = Config::default();
    r.absorb_disk(&c.to_disk_json());
    assert_eq!(r.presets, c.presets);
}

#[test]
fn per_model_temperature_and_prices_follow_the_model() {
    let v = serde_json::json!({
        "provider": "anthropic",
        "model": "claude-haiku-4-5",
        "temperature": 0.5,
        "models": {
            "claude-haiku-4-5": {"price_in": 1.0, "price_out": 5.0, "temperature": 0.9},
            "local-gemma": {"price_in": 0.0, "price_out": 0.0}
        }
    });
    let mut c = Config::default();
    c.absorb_disk(&v);
    c.sync_prices();
    assert_eq!(c.temperature(), Some(0.9));
    let p = c.effective_price_list().unwrap();
    assert_eq!((p.price_in, p.price_out), (1.0, 5.0));
    assert_eq!(p.price_cached_in, built_in_prices("anthropic").unwrap().price_cached_in, "unset fields come from the provider");
    assert_eq!(c.price_in, 1.0, "the mirror fields follow the model");
    // the provider's own list is untouched, and that's what the file mirrors
    assert_eq!(c.price_list().unwrap().price_in, 15.0);
    let json = c.to_disk_json();
    assert_eq!(json["price_in"], 15.0, "{json}");
    let mut back = Config::default();
    back.absorb_disk(&json);
    assert_eq!(back.price_list().unwrap().price_in, 15.0, "a model's price never leaks into the provider's list");
    // another model on the same provider: provider prices, global temperature
    c.model = "claude-opus".into();
    c.sync_prices();
    assert_eq!((c.price_in, c.temperature()), (15.0, Some(0.5)));
    // a custom server without a list still gets a cost readout for a priced model
    c.provider = "box".into();
    c.model = "local-gemma".into();
    assert_eq!(c.price_list(), None);
    assert_eq!(c.effective_price_list().map(|p| p.price_in), Some(0.0));
    c.model = "other".into();
    assert_eq!(c.effective_price_list(), None);
}

#[test]
fn the_wire_protocol_follows_the_provider() {
    // legacy top-level `api` belongs to the provider active when written
    let v = serde_json::json!({"provider": "vercel", "model": "m", "api": "messages"});
    let mut c = Config::default();
    c.absorb_disk(&v);
    assert!(matches!(c.api_kind(), ApiKind::Messages));
    assert_eq!(c.apis.get("vercel").map(String::as_str), Some("messages"));
    c.provider = "box".into();
    assert!(matches!(c.api_kind(), ApiKind::Chat), "a local server doesn't inherit the gateway's protocol");
    c.provider = "anthropic".into();
    assert!(matches!(c.api_kind(), ApiKind::Messages));
    let json = c.to_disk_json();
    assert!(json.get("api").is_none());
    assert_eq!(json["apis"]["vercel"], "messages");
}

#[test]
fn context_window_table_by_model() {
    assert_eq!(known_context_window("deepseek-v4-pro"), 1_000_000);
    assert_eq!(known_context_window("deepseek-v4-flash"), 1_000_000);
    // `deepseek-flash` is the API's own alias for v4 Flash — also 1M.
    assert_eq!(known_context_window("deepseek-flash"), 1_000_000);
    assert_eq!(known_context_window("deepseek-chat"), 128_000);
    assert_eq!(known_context_window("deepseek-reasoner"), 128_000);
    // A non-DeepSeek "flash" (GLM-4-Flash) must not inherit the 1M window.
    assert_eq!(known_context_window("glm-4-flash"), 128_000);
    // Unknown models fall back to the conservative 128k, not the 1M default.
    assert_eq!(known_context_window("gpt-4o-mini"), 128_000);
    // A fresh config auto-derives, and stays non-explicit so it keeps
    // tracking the model and never gets written to disk.
    let c = Config::default();
    assert!(!c.context_window_explicit);
}

#[test]
fn env_keys_are_provider_scoped() {
    // Only the configured provider's var (plus the universal override)
    // can supply the key — never another provider's.
    for (name, _, _) in PROVIDERS {
        let vars = key_env_vars(name);
        assert_eq!(vars[0], "HAIKUCODE_API_KEY", "{name}: override must win");
        for v in &vars[1..] {
            let prov_upper = name.to_uppercase();
            assert!(
                v.starts_with(&prov_upper)
                    || (*name == "google" && v.starts_with("GEMINI"))
                    || (*name == "zai" && (v.starts_with("ZAI") || v.starts_with("ZHIPUAI")))
                    || (*name == "vercel" && *v == "AI_GATEWAY_API_KEY"),
                "{name} must not read {v}"
            );
        }
    }
    // Unknown/custom providers get only the explicit override.
    assert_eq!(key_env_vars("my-llm-box"), &["HAIKUCODE_API_KEY"]);
}
