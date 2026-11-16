//! The `haikucode sessions` and `haikucode model` subcommands.

use crate::config::Config;
use crate::{api, config, sessions};
use std::io::Write;

/// `haikucode sessions [--json]` lists this directory's sessions, newest first;
/// `haikucode sessions delete <id|name>` removes one. Returns the exit status.
pub(super) fn sessions_subcommand(args: &[String], json: bool) -> i32 {
    let store = sessions::Store::here();
    if args.first().map(String::as_str) == Some("delete") {
        let Some(key) = args.get(1) else {
            eprintln!("usage: haikucode sessions delete <id|name>");
            return 2;
        };
        return match store.find(key) {
            Ok((m, p)) => {
                sessions::delete(&p);
                println!("deleted {} ({})", m.id, m.label());
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
    }
    let list = store.list();
    if json {
        let metas: Vec<&sessions::Meta> = list.iter().map(|(m, _)| m).collect();
        println!("{}", serde_json::to_string_pretty(&metas).unwrap_or_default());
        return 0;
    }
    if list.is_empty() {
        println!("no saved sessions in {}", store.cwd.display());
        return 0;
    }
    let now = config::now_unix();
    for (m, _) in &list {
        println!("{}", sessions::summary_line(m, now, false));
    }
    0
}

pub(super) fn model_subcommand(cfg: &Config, arg: Option<String>) {
    let http = api::agent_http();
    if let Some(id) = arg {
        let mut c = cfg.clone();
        c.model = id.clone();
        c.persist_model();
        println!("model set to {id}");
        return;
    }
    if !cfg.has_credentials() {
        eprintln!("{} isn't set up (no API key or sign-in). Run: haikucode --config", cfg.provider);
        return;
    }
    match api::list_models(&http, cfg) {
        Ok(ids) if !ids.is_empty() => {
            println!("available models ({}):", cfg.provider);
            for (i, id) in ids.iter().enumerate() {
                let cur = if *id == cfg.model { "  <- current" } else { "" };
                println!("  {:>2}) {id}{cur}", i + 1);
            }
            print!("pick 1-{} or id (blank=keep {}): ", ids.len(), cfg.model);
            let _ = std::io::stdout().flush();
            let mut sel = String::new();
            let _ = std::io::stdin().read_line(&mut sel);
            let sel = sel.trim();
            if sel.is_empty() {
                return;
            }
            let chosen = match sel.parse::<usize>() {
                Ok(n) if n >= 1 && n <= ids.len() => ids[n - 1].clone(),
                _ => sel.to_string(),
            };
            let mut c = cfg.clone();
            c.model = chosen.clone();
            c.persist_model();
            println!("model set to {chosen}");
        }
        Ok(_) => println!("provider returned no models."),
        Err(e) => eprintln!("could not fetch models: {e}"),
    }
}
