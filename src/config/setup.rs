//! The interactive first-run setup wizard (cooked stdin, before the TUI).

use super::*;

/// Interactive first-run setup, executed before the TUI starts (cooked stdin):
/// the same three steps as `/provider` — provider, sign-in, model.
pub fn run_setup() -> Result<Config> {
    let mut cfg = Config::load();
    let bold = |t: &str| println!("\n\x1b[1m{t}\x1b[0m");
    let dim = |t: &str| println!("\x1b[2m  {t}\x1b[0m");
    let err = |t: &str| println!("\x1b[31m  {t}\x1b[0m");
    println!("\x1b[1mhaikucode setup\x1b[0m  \x1b[2m(Enter takes the [default]; Ctrl+C quits)\x1b[0m");

    // Step 1: provider.
    bold("Step 1 of 3 · Provider");
    let width = PROVIDERS.iter().map(|(n, _, _)| n.len()).max().unwrap_or(8);
    let current = PROVIDERS.iter().position(|(n, _, _)| *n == cfg.provider).unwrap_or(0);
    for (i, (name, base, _)) in PROVIDERS.iter().enumerate() {
        let ways = auth_methods(name)
            .iter()
            .map(|m| method_short(name, *m))
            .collect::<Vec<_>>()
            .join(" or ");
        let host = base.split('/').nth(2).unwrap_or(base);
        println!("  {:>2}) {name:<width$}  \x1b[2m{host} · {ways}\x1b[0m", i + 1);
    }
    let custom_n = PROVIDERS.len() + 1;
    println!("  {custom_n:>2}) {:<width$}  \x1b[2many OpenAI-compatible URL (LiteLLM, Ollama, LM Studio, …)\x1b[0m", "custom");
    let n = loop {
        let choice = prompt(&format!("> [{}] ", current + 1))?;
        if choice.is_empty() {
            break current + 1;
        }
        match choice.parse::<usize>() {
            Ok(n) if (1..=custom_n).contains(&n) => break n,
            _ => match PROVIDERS.iter().position(|(p, _, _)| *p == choice) {
                Some(i) => break i + 1,
                None => err(&format!("pick 1 to {custom_n}, or type a provider's name")),
            },
        }
    };
    let custom = n == custom_n;
    let (prov, base, default_model): (String, String, String) = if custom {
        let p = loop {
            let p = prompt("  name [custom]: ")?;
            let p = nonempty(p, "custom");
            match valid_custom_name(&p) {
                Ok(()) => break p,
                Err(e) => err(&e),
            }
        };
        let saved = cfg.custom_provider(&p).cloned();
        let b = loop {
            let hint = saved.as_ref().map(|c| format!(" [{}]", c.base_url)).unwrap_or_default();
            dim("the OpenAI-compatible endpoint, e.g. http://localhost:11434/v1");
            let b = prompt(&format!("  base URL{hint}: "))?;
            let b = nonempty(b, saved.as_ref().map(|c| c.base_url.as_str()).unwrap_or(""));
            match normalize_provider_url(&b) {
                Ok(b) => break b,
                Err(e) => err(&e),
            }
        };
        (p, b, saved.map(|c| c.model).unwrap_or_default())
    } else {
        let (p, b, m) = PROVIDERS[n - 1];
        (p.into(), b.into(), m.into())
    };
    cfg.provider = prov.clone();
    cfg.base_url = base.clone();
    cfg.resolve_key();

    // Step 2: sign in.
    bold(&format!("Step 2 of 3 · Sign in to {prov}"));
    let methods = auth_methods(&prov);
    if let Some(why) = no_sub_reason(&prov) {
        dim(why);
    }
    let method = if methods.len() == 1 {
        methods[0]
    } else {
        let cur = methods.iter().position(|m| *m == cfg.auth_for(&prov)).unwrap_or(0);
        for (i, m) in methods.iter().enumerate() {
            println!("  {}) {}", i + 1, method_long(&prov, *m, &cfg));
        }
        loop {
            let c = prompt(&format!("> [{}] ", cur + 1))?;
            if c.is_empty() {
                break methods[cur];
            }
            match c.parse::<usize>() {
                Ok(i) if (1..=methods.len()).contains(&i) => break methods[i - 1],
                _ => err(&format!("pick 1 to {}", methods.len())),
            }
        }
    };
    if method == AuthMethod::Api {
        cfg.auth_modes.remove(&prov);
    } else {
        cfg.auth_modes.insert(prov.clone(), method.id().to_string());
    }
    cfg.sync_auth_mode();
    match method {
        AuthMethod::Api | AuthMethod::Plan => {
            if let Some(url) = key_url(&prov, method == AuthMethod::Plan) {
                dim(&format!("get one at {url}"));
            }
            let saved = cfg.api_keys.get(&prov).filter(|k| !k.is_empty()).map(|k| key_tail(k));
            let label = if method == AuthMethod::Plan { coding_plan_label(&prov) } else { "API" };
            let key = loop {
                let hint = match &saved {
                    Some(t) => format!(" [saved …{t}, Enter keeps it]"),
                    None if custom => " [none]".into(),
                    None => String::new(),
                };
                let k = prompt_secret(&format!("  {label} key{hint} (hidden): "))?;
                if !k.is_empty() || saved.is_some() || custom {
                    break k;
                }
                err("paste a key (or Ctrl+C and pick another provider)");
            };
            if !key.is_empty() {
                cfg.api_keys.insert(prov.clone(), key.clone());
                cfg.api_key = key;
                cfg.key_from_env = false;
            }
        }
        AuthMethod::Sub => {
            let p = crate::auth::provider(&prov).ok_or_else(|| anyhow::anyhow!("{prov} has no subscription sign-in"))?;
            let login = crate::auth::start(p)?;
            println!("  Opening your browser to sign in with {}.", crate::auth::sub_label(&prov));
            dim(&format!("if it didn't open, visit: {}", login.url));
            let token = login.finish()?;
            cfg.oauth.insert(prov.clone(), token);
            println!("\x1b[32m  signed in\x1b[0m");
        }
        AuthMethod::None => dim("no key: requests go out without an Authorization header"),
    }

    // Step 3: model, from the server's list when it answers.
    bold("Step 3 of 3 · Model");
    let default_model = if cfg.chatgpt_sub() { String::new() } else { default_model };
    let ids = match crate::api::list_models(&crate::api::agent_http(), &cfg) {
        Ok(ids) => crate::api::chat_models(ids),
        Err(e) => {
            err(&format!("couldn't load the model list: {}", crate::api::truncate(&e.to_string(), 200)));
            if matches!(method, AuthMethod::Api | AuthMethod::Plan) && e.to_string().contains("HTTP 401") {
                dim("the key was refused: run `haikucode --config` again to re-enter it");
            }
            Vec::new()
        }
    };
    let default_model = if default_model.is_empty() || (!ids.is_empty() && !ids.contains(&default_model)) {
        ids.first().cloned().unwrap_or(default_model)
    } else {
        default_model
    };
    const SHOWN: usize = 30;
    for (i, id) in ids.iter().take(SHOWN).enumerate() {
        let mark = if *id == default_model { "  (default)" } else { "" };
        println!("  {:>2}) {id}{mark}", i + 1);
    }
    if ids.len() > SHOWN {
        dim(&format!("…and {} more: type any model id", ids.len() - SHOWN));
    }
    let model = loop {
        let hint = if default_model.is_empty() { String::new() } else { format!(" [{default_model}]") };
        let ask = if ids.is_empty() { "  model id" } else { "  number or model id" };
        let m = prompt(&format!("{ask}{hint}: "))?;
        let m = match m.parse::<usize>() {
            Ok(i) if (1..=ids.len().min(SHOWN)).contains(&i) => ids[i - 1].clone(),
            _ => nonempty(m, &default_model),
        };
        if !m.is_empty() {
            break m;
        }
        err("a model id is required");
    };
    cfg.model = model;
    if custom {
        cfg.custom_providers.retain(|c| c.name != cfg.provider);
        cfg.custom_providers.push(CustomProvider {
            name: cfg.provider.clone(),
            base_url: cfg.base_url.clone(),
            model: cfg.model.clone(),
        });
    }
    if !cfg.context_window_explicit {
        cfg.context_window = known_context_window(&cfg.model);
    }
    cfg.sync_prices();
    cfg.save()?;
    println!(
        "\n\x1b[32m✓ {} · {} · {}\x1b[0m  \x1b[2msaved {}\x1b[0m",
        cfg.provider,
        method_short(&cfg.provider, method),
        cfg.model,
        config_path().display()
    );
    Ok(cfg)
}

/// A sign-in method in a few words, e.g. "API key" or "ChatGPT sign-in".
pub fn method_short(provider: &str, m: AuthMethod) -> String {
    match m {
        AuthMethod::Api => "API key".into(),
        AuthMethod::Sub => format!("{} sign-in", crate::auth::sub_label(provider)),
        AuthMethod::Plan => coding_plan_label(provider).into(),
        AuthMethod::None => "no key".into(),
    }
}

/// A sign-in method as a menu line, with what's already saved for it.
pub fn method_long(provider: &str, m: AuthMethod, cfg: &Config) -> String {
    let saved_key = cfg.api_keys.get(provider).filter(|k| !k.is_empty()).map(|k| key_tail(k));
    match m {
        AuthMethod::Api => match saved_key {
            Some(t) if cfg.auth_for(provider) == AuthMethod::Api => format!("API key — saved …{t}"),
            _ => "API key — pay as you go".into(),
        },
        AuthMethod::Sub if cfg.signed_in(provider) => {
            format!("{} subscription — signed in", crate::auth::sub_label(provider))
        }
        AuthMethod::Sub => match provider {
            "openai" => "ChatGPT subscription (Plus / Pro / Business) — sign in in your browser".into(),
            _ => "subscription — sign in in your browser".into(),
        },
        AuthMethod::Plan => match saved_key {
            Some(t) if cfg.auth_for(provider) == AuthMethod::Plan => {
                format!("{} — saved key …{t}", coding_plan_label(provider))
            }
            _ => format!("{} — a subscription key, on the plan's own endpoint", coding_plan_label(provider)),
        },
        AuthMethod::None => "no key — a local or open server".into(),
    }
}

/// The last four characters of a key, for "saved …abcd".
pub fn key_tail(k: &str) -> String {
    let n = k.chars().count();
    k.chars().skip(n.saturating_sub(4)).collect()
}

/// `prompt` without echoing what's typed (`stty -echo`), for keys.
fn prompt_secret(msg: &str) -> Result<String> {
    use std::process::{Command, Stdio};
    let stty = |arg: &str| {
        Command::new("stty")
            .arg(arg)
            .stdin(Stdio::inherit())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    let hidden = stty("-echo");
    let line = prompt(msg);
    if hidden {
        stty("echo");
        println!();
    }
    line
}

fn prompt(msg: &str) -> Result<String> {
    print!("{msg}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn nonempty(s: String, fallback: &str) -> String {
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}
