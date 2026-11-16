//! The provider account: balance refresh and subscription sign-in.

use super::{UiEvent, Worker};
use crate::api;
use crate::config::Config;
use std::sync::atomic::Ordering;

impl Worker {
    /// Fetch the account balance on a background thread (best-effort).
    pub(super) fn refresh_balance(&self) {
        let gen = self.balance_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.ui.send(UiEvent::BalanceCleared);
        let ui = self.ui.clone();
        let cfg = self.cfg.clone();
        let current = self.balance_gen.clone();
        std::thread::spawn(move || {
            let http = api::agent_http();
            if let Some(b) = api::fetch_balance(&http, &cfg) {
                if current.load(Ordering::SeqCst) != gen {
                    return; // a newer provider/key took over meanwhile
                }
                // An account billed in another currency (DeepSeek's China
                // region is CNY) shows in the status line's currency at the
                // day's rate; unconverted (and tagged) if the rate is missing.
                let want = crate::money::Currency::parse(&cfg.price_currency);
                let b = if b.currency == want {
                    b
                } else {
                    api::exchange_rate(&http, &b.currency.code, &want.code)
                        .and_then(|rate| b.convert(&want, rate))
                        .unwrap_or(b)
                };
                if current.load(Ordering::SeqCst) == gen {
                    let _ = ui.send(UiEvent::Balance(b));
                }
            }
        });
    }

    /// Run the OAuth subscription flow for `name` and store the token. Blocks
    /// the worker (a deliberate modal action) while the browser round-trips;
    /// Esc (the turn's cancel flag) gives up. The UI hears `SignedIn` or
    /// `SignInFailed` and carries on with the setup.
    pub(super) fn login(&mut self, name: &str) {
        let fail = |w: &Self, error: String| {
            let _ = w.ui.send(UiEvent::SignInFailed { provider: name.to_string(), error });
        };
        let Some(p) = crate::auth::provider(name) else {
            return fail(self, format!("{name} has no subscription sign-in; use an API key"));
        };
        let login = match crate::auth::start(p) {
            Ok(l) => l,
            Err(e) => return fail(self, e.to_string()),
        };
        let _ = self.ui.send(UiEvent::Notice(format!(
            "Opening your browser to sign in with {}. Waiting for it (Esc cancels).\nIf it didn't open, visit:\n{}",
            crate::auth::sub_label(name),
            login.url
        )));
        match login.finish_or_cancel(&self.cancel) {
            Ok(token) => {
                self.cfg.oauth.insert(name.to_string(), token.clone());
                Config::persist_oauth(name, &token);
                let _ = self.ui.send(UiEvent::SignedIn { provider: name.to_string(), token });
            }
            Err(e) => fail(self, e.to_string()),
        }
    }

    /// Refresh the active provider's subscription token if it's near expiry, so
    /// a resumed session stays authenticated. Best-effort and synchronous.
    pub(super) fn ensure_oauth_fresh(&mut self) {
        let name = self.cfg.provider.clone();
        let Some(token) = self.cfg.oauth.get(&name).cloned() else { return };
        if !token.needs_refresh() {
            return;
        }
        let Some(p) = crate::auth::provider(&name) else { return };
        if let Ok(fresh) = crate::auth::refresh(&p, &token) {
            self.cfg.oauth.insert(name.clone(), fresh.clone());
            Config::persist_oauth(&name, &fresh);
        }
    }
}
