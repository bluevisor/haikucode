//! Reading and writing config.json: the lenient loader, env overrides,
//! the minimal on-disk JSON, and single-field persist helpers.

use super::*;

impl Config {
    /// Read config.json leniently: each known field is merged independently,
    /// so one malformed field (a typo in mcp_servers, say) can't reset the
    /// others. No env override — this is the on-disk truth, safe to rewrite.
    /// Read every key haikucode understands out of one config.json document.
    pub(super) fn absorb_disk(&mut self, v: &serde_json::Value) {
        let cfg = self;
        if let Some(s) = v.get("provider").and_then(|x| x.as_str()) {
            cfg.provider = s.to_string();
        }
        if let Some(s) = v.get("base_url").and_then(|x| x.as_str()) {
            cfg.base_url = s.to_string();
        }
        if let Some(s) = v.get("model").and_then(|x| x.as_str()) {
            cfg.model = s.to_string();
        }
        // New-style per-provider keys (api_keys object).
        if let Some(map) = v.get("api_keys").and_then(|x| x.as_object()) {
            for (k, val) in map {
                if let Some(s) = val.as_str() {
                    cfg.api_keys.insert(k.clone(), s.to_string());
                }
            }
        }
        // Subscription-login tokens (one per provider).
        // One malformed entry must not take its siblings with it: parse
        // entry by entry, and keep the raw block in `extra` so a later
        // model/theme rewrite preserves the broken entry verbatim
        // instead of dropping the whole map from disk.
        if let Some(m) = v.get("oauth") {
            let mut broken = false;
            if let Some(obj) = m.as_object() {
                for (k, val) in obj {
                    match serde_json::from_value::<OAuthToken>(val.clone()) {
                        Ok(t) => {
                            cfg.oauth.insert(k.clone(), t);
                        }
                        Err(_) => broken = true,
                    }
                }
            } else {
                broken = true;
            }
            if broken {
                cfg.extra.insert("oauth".into(), m.clone());
            }
        }
        // Legacy single api_key field — use as fallback for the current
        // provider if no per-provider entry exists.
        if let Some(s) = v.get("api_key").and_then(|x| x.as_str()) {
            if !cfg.api_keys.contains_key(&cfg.provider) {
                cfg.api_keys.insert(cfg.provider.clone(), s.to_string());
            }
        }
        if let Some(s) = v.get("theme").and_then(|x| x.as_str()) {
            let s = s.to_string();
            // Validate; unknown theme names silently fall back to default
            // so a hand-edited config can't cause a broken UI.
            cfg.theme = if crate::ui::is_theme_name(&s) { s } else { default_theme() };
        }
        if let Some(n) = v.get("context_window").and_then(|x| x.as_u64()) {
            cfg.context_window = n as u32;
            cfg.context_window_explicit = true;
        }
        // Per-model overrides; one malformed entry is skipped on its own.
        if let Some(m) = v.get("models") {
            match m.as_object() {
                Some(obj) => {
                    for (k, val) in obj {
                        match serde_json::from_value::<ModelSettings>(val.clone()) {
                            Ok(mut ms) => {
                                ms.context_window = ms.context_window.filter(|n| *n > 0);
                                if !ms.is_empty() {
                                    cfg.models.insert(k.clone(), ms);
                                }
                            }
                            Err(e) => eprintln!("config: models[{k:?}] is malformed ({e}) and was skipped"),
                        }
                    }
                }
                None => eprintln!("config: models is not an object and was ignored"),
            }
        }
        // Legacy (pre-`models`): a bare per-model context-window map.
        if let Some(obj) = v.get("model_context_windows").and_then(|m| m.as_object()) {
            for (k, val) in obj {
                if let Some(n) = val.as_u64().filter(|n| *n > 0) {
                    let e = cfg.models.entry(k.clone()).or_default();
                    e.context_window.get_or_insert(n.min(u32::MAX as u64) as u32);
                }
            }
        }
        if let Some(n) = v.get("price_in").and_then(|x| x.as_f64()) {
            cfg.price_in = n;
        }
        if let Some(n) = v.get("price_out").and_then(|x| x.as_f64()) {
            cfg.price_out = n;
        }
        if let Some(s) = v.get("price_currency").and_then(|x| x.as_str()) {
            let s = s.trim().to_ascii_uppercase();
            // An empty or junk value keeps the documented default rather
            // than producing a unitless (or bogus-symbol) readout.
            cfg.price_currency = if s.is_empty() {
                default_price_currency()
            } else {
                s
            };
        }
        if let Some(m) = v.get("mcp_servers") {
            let mut broken = false;
            if let Some(obj) = m.as_object() {
                for (k, val) in obj {
                    match McpServerConfig::parse(val) {
                        Some(c) => {
                            cfg.mcp_servers.insert(k.clone(), c);
                        }
                        None => {
                            eprintln!("config: mcp_servers.{k} is malformed and was skipped");
                            broken = true;
                        }
                    }
                }
            } else {
                broken = true;
            }
            if broken {
                cfg.extra.insert("mcp_servers".into(), m.clone());
            }
        }
        if let Some(b) = v.get("update_check").and_then(|x| x.as_bool()) {
            cfg.update_check = b;
        }
        if let Some(b) = v.get("show_thinking").and_then(|x| x.as_bool()) {
            cfg.show_thinking = b;
        }
        if let Some(b) = v.get("auto_commit").and_then(|x| x.as_bool()) {
            cfg.auto_commit = b;
        }
        if let Some(list) = v.get("statusline").and_then(|x| x.as_array()) {
            cfg.statusline = list.iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
        }
        // The old `thinking` switch only ever reached DeepSeek and GLM
        // (as their `thinking` field); every other provider ignored it.
        // Keep that: "medium" there, nothing sent elsewhere — mapping it
        // to "medium" for a local Qwen would switch reasoning on that
        // the user never actually had.
        if let Some(true) = v.get("thinking").and_then(|x| x.as_bool()) {
            if matches!(cfg.provider.as_str(), "deepseek" | "zai") {
                cfg.reasoning = "medium".into();
            }
        }
        if let Some(s) = v.get("permission").and_then(|x| x.as_str()) {
            cfg.permission = s.to_string();
        }
        // The old global `auth_mode` is deliberately not read: it predates
        // per-provider sign-in, and a stale "sub" would follow every provider.
        if let Some(map) = v.get("auth_modes").and_then(|x| x.as_object()) {
            for (k, val) in map {
                if let Some(s) = val.as_str() {
                    cfg.auth_modes.insert(k.clone(), s.to_string());
                }
            }
        }
        if let Some(n) = v.get("max_tool_calls").and_then(|x| x.as_u64()) {
            cfg.max_tool_calls = n as u32;
        }
        if let Some(t) = v.get("temperature").and_then(|x| x.as_f64()) {
            cfg.temperature = Some(t);
        }
        if let Some(r) = v.get("reasoning").and_then(|x| x.as_str()) {
            cfg.reasoning = normalize_reasoning(r);
        }
        if let Some(obj) = v.get("apis").and_then(|x| x.as_object()) {
            for (k, a) in obj {
                if let Some(a) = a.as_str().map(str::trim).filter(|a| !a.is_empty()) {
                    cfg.apis.insert(k.clone(), a.to_string());
                }
            }
        }
        // Legacy: one top-level `api`, which belonged to the provider active
        // when it was written.
        if let Some(a) = v.get("api").and_then(|x| x.as_str()).map(str::trim).filter(|a| !a.is_empty()) {
            let provider = cfg.provider.clone();
            cfg.apis.entry(provider).or_insert_with(|| a.to_string());
        }
        if let Some(p) = v.get("price_cached_in").and_then(|x| x.as_f64()) {
            cfg.price_cached_in = Some(p);
        }
        // Per-provider price lists (the cost counter follows the provider).
        // One malformed entry must not take its siblings with it; the raw
        // block goes to `extra` so a rewrite keeps it verbatim.
        if let Some(m) = v.get("prices") {
            let mut broken = false;
            if let Some(obj) = m.as_object() {
                for (k, val) in obj {
                    match serde_json::from_value::<PriceList>(val.clone()) {
                        Ok(p) => {
                            cfg.prices.insert(k.clone(), p);
                        }
                        Err(_) => broken = true,
                    }
                }
            } else {
                broken = true;
            }
            if broken {
                cfg.extra.insert("prices".into(), m.clone());
            }
        }
        if let Some(obj) = v.get("presets").and_then(|x| x.as_object()) {
            for (k, val) in obj {
                if !is_builtin_provider(k) {
                    continue;
                }
                if let Ok(p) = serde_json::from_value::<PresetState>(val.clone()) {
                    cfg.presets.insert(k.clone(), p);
                }
            }
        }
        if let Some(list) = v.get("custom_providers").and_then(|x| x.as_array()) {
            for item in list {
                match serde_json::from_value::<CustomProvider>(item.clone()) {
                    Ok(cp) if valid_custom_name(&cp.name).is_ok() && cp.base_url.starts_with("http") => {
                        cfg.custom_providers.retain(|c| c.name != cp.name);
                        cfg.custom_providers.push(cp);
                    }
                    _ => eprintln!("config: a custom_providers entry is malformed and was skipped"),
                }
            }
        }
        cfg.adopt_active_custom();
        // Legacy: the single top-level price list belonged to the provider
        // active when it was written. Fold it into the per-provider map so
        // switching providers and back keeps it.
        if !cfg.prices.contains_key(&cfg.provider)
            && (cfg.price_in != default_price_in()
                || cfg.price_out != default_price_out()
                || cfg.price_currency != default_price_currency()
                || cfg.price_cached_in.is_some())
        {
            let mut list = PriceList {
                price_in: cfg.price_in,
                price_out: cfg.price_out,
                price_cached_in: cfg.price_cached_in,
                price_currency: cfg.price_currency.clone(),
            };
            // Legacy files predate the per-provider cache price: keep the
            // built-in one (e.g. DeepSeek's 0.003 cache-hit price).
            if list.price_cached_in.is_none() {
                list.price_cached_in = built_in_prices(&cfg.provider).and_then(|b| b.price_cached_in);
            }
            cfg.prices.insert(cfg.provider.clone(), list);
        }
        for key in [
            "permissions",
            "hooks",
            "auto_bump_version",
            "shared_skills",
            "sandbox",
            "sandbox_network",
            "sandbox_allow_write",
            "web_search",
            "web_search_url",
            "web_search_key",
        ] {
            if let Some(x) = v.get(key) {
                cfg.extra.insert(key.to_string(), x.clone());
            }
        }
    }

    pub(super) fn load_disk() -> Config {
        let mut cfg = Config::default();
        if let Ok(text) = std::fs::read_to_string(config_path()) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                cfg.absorb_disk(&v);
            }
        }
        // Resolve the effective key from the per-provider map.
        cfg.resolve_key();
        cfg.sync_auth_mode();
        // The top-level price fields mirror the active provider's list.
        cfg.sync_prices();
        cfg
    }

    pub fn load() -> Config {
        let mut cfg = Self::load_disk();
        // First match wins, so HAIKUCODE_API_KEY (listed first) overrides the
        // provider-specific var.
        for var in key_env_vars(&cfg.provider) {
            if let Ok(k) = std::env::var(var) {
                if !k.is_empty() {
                    cfg.api_key = k;
                    cfg.key_from_env = true;
                    break;
                }
            }
        }
        // When the user hasn't pinned a window, derive it from the model so the
        // ctx bar and compaction limit match the model's real capacity rather
        // than a stale 128k default. The worker refines this from the provider's
        // /models metadata (for providers that advertise it) once it's running.
        if !cfg.context_window_explicit {
            cfg.context_window = known_context_window(&cfg.model);
        }
        cfg
    }

    /// Persist to disk with 0600 perms. An env-injected key is never written.
    pub fn save(&self) -> Result<()> {
        // UI tests flip the theme, permission mode and brief view, each of
        // which saves; never let them rewrite the real ~/config file.
        if cfg!(test) {
            return Ok(());
        }
        let dir = config_dir();
        std::fs::create_dir_all(&dir).context("create config dir")?;
        let json = self.to_disk_json();
        let path = config_path();
        atomic_write(&path, serde_json::to_string_pretty(&json)?.as_bytes())
            .context("write config")?;
        set_private(&path);
        Ok(())
    }

    /// The exact JSON `save` writes: the core fields plus every user
    /// customization that differs from its default, so a model/theme rewrite
    /// can't drop the rest of the file. Split out from `save` so the
    /// preserve-what-the-user-set rules are testable without touching disk.
    pub(super) fn to_disk_json(&self) -> serde_json::Value {
        let mut on_disk = self.clone();
        // Update the per-provider map with the current effective key (unless env-sourced).
        if !self.key_from_env && !self.api_key.is_empty() {
            on_disk.api_keys.insert(self.provider.clone(), self.api_key.clone());
        }
        let mut json = serde_json::json!({
            "provider": on_disk.provider,
            "base_url": on_disk.base_url,
            "model": on_disk.model,
            "theme": on_disk.theme,
        });
        // Write per-provider keys (the canonical store). Also write the legacy
        // api_key field with the current provider's key for older haikucode readers.
        if !on_disk.api_keys.is_empty() {
            json["api_keys"] = serde_json::to_value(&on_disk.api_keys).unwrap_or_default();
            if let Some(k) = on_disk.api_keys.get(&on_disk.provider) {
                if !self.key_from_env {
                    json["api_key"] = serde_json::json!(k);
                }
            }
        }
        // Subscription-login tokens, when any provider has been logged in.
        if !on_disk.oauth.is_empty() {
            json["oauth"] = serde_json::to_value(&on_disk.oauth).unwrap_or_default();
        }
        // Preserve user customizations across model/theme rewrites; only
        // non-default values are written, keeping the common file minimal.
        if !on_disk.mcp_servers.is_empty() {
            json["mcp_servers"] = serde_json::to_value(&on_disk.mcp_servers).unwrap_or_default();
        }
        if !on_disk.update_check {
            json["update_check"] = serde_json::json!(false);
        }
        if on_disk.show_thinking {
            json["show_thinking"] = serde_json::json!(true);
        }
        if !on_disk.statusline.is_empty() {
            json["statusline"] = serde_json::json!(on_disk.statusline);
        }
        if !on_disk.auto_commit {
            json["auto_commit"] = serde_json::json!(false);
        }
        // Only persist a window the user pinned; an auto-derived value stays out
        // of the file so it keeps tracking the model on later launches.
        if on_disk.context_window_explicit {
            json["context_window"] = serde_json::json!(on_disk.context_window);
        }
        let models: BTreeMap<&String, &ModelSettings> = on_disk.models.iter().filter(|(_, m)| !m.is_empty()).collect();
        if !models.is_empty() {
            json["models"] = serde_json::to_value(&models).unwrap_or_default();
        }
        // The top-level price fields mirror the active provider's list (not a
        // model override, which lives in `models` and would otherwise be
        // folded into the provider's list on the next load). Skip them when
        // the provider has no list (a custom server with a hidden cost) so a
        // reload doesn't mistake the zeros for a user price list.
        if let Some(p) = on_disk.price_list() {
            if p.price_in != default_price_in() {
                json["price_in"] = serde_json::json!(p.price_in);
            }
            if p.price_out != default_price_out() {
                json["price_out"] = serde_json::json!(p.price_out);
            }
            if p.price_currency.to_ascii_uppercase() != default_price_currency() {
                json["price_currency"] = serde_json::json!(p.price_currency);
            }
            if let Some(c) = p.price_cached_in {
                json["price_cached_in"] = serde_json::json!(c);
            }
        }
        if !on_disk.prices.is_empty() {
            json["prices"] = serde_json::to_value(&on_disk.prices).unwrap_or_default();
        }
        if on_disk.permission != default_permission() {
            json["permission"] = serde_json::json!(on_disk.permission);
        }
        if !on_disk.auth_modes.is_empty() {
            json["auth_modes"] = serde_json::to_value(&on_disk.auth_modes).unwrap_or_default();
        }
        if on_disk.max_tool_calls != default_max_tool_calls() {
            json["max_tool_calls"] = serde_json::json!(on_disk.max_tool_calls);
        }
        if let Some(t) = on_disk.temperature {
            json["temperature"] = serde_json::json!(t);
        }
        if on_disk.reasoning != default_reasoning() {
            json["reasoning"] = serde_json::json!(on_disk.reasoning);
        }
        if !on_disk.apis.is_empty() {
            json["apis"] = serde_json::to_value(&on_disk.apis).unwrap_or_default();
        }
        if !on_disk.custom_providers.is_empty() {
            json["custom_providers"] = serde_json::to_value(&on_disk.custom_providers).unwrap_or_default();
        }
        if !on_disk.presets.is_empty() {
            json["presets"] = serde_json::to_value(&on_disk.presets).unwrap_or_default();
        }
        for (k, v) in &on_disk.extra {
            json[k.as_str()] = v.clone();
        }
        json
    }

    /// Persist only the show-thinking toggle, preserving the rest of the file.
    pub fn persist_show_thinking(on: bool) {
        let mut disk = Config::load_disk();
        disk.show_thinking = on;
        let _ = disk.save();
    }

    /// Persist only the status-row segments (empty = default), preserving the
    /// rest of the file.
    pub fn persist_statusline(segments: &[String]) {
        let mut disk = Config::load_disk();
        disk.statusline = segments.to_vec();
        let _ = disk.save();
    }

    /// Persist only the theme name, preserving the rest of the file.
    pub fn persist_theme(theme: &str) {
        let mut disk = Config::load_disk();
        disk.theme = theme.to_string();
        let _ = disk.save();
    }
}

#[cfg(unix)]
fn set_private(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn set_private(_path: &std::path::Path) {}
