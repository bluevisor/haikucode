//! Provider metadata endpoints: model lists, context windows, account balance
//! and exchange rates.

use super::{authed, truncate};
use crate::config::{ApiKind, Config};
use crate::money::{Balance, Currency};
use anyhow::{anyhow, Result};

/// DeepSeek account balance (best-effort; returns None for other providers).
pub fn fetch_balance(http: &ureq::Agent, cfg: &Config) -> Option<Balance> {
    let url = format!("{}/user/balance", cfg.base_url.trim_end_matches('/'));
    let text = http
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .set("Authorization", &format!("Bearer {}", cfg.bearer()))
        .call()
        .ok()?
        .into_string()
        .ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    pick_balance(&v, &Currency::parse(&cfg.price_currency))
}

/// Units of `to` per one `from`, from the ECB reference rates (Frankfurter:
/// free, no key), cached for the session: the balance refreshes every turn, the
/// rate needn't. Lets a balance billed in CNY show in the USD status line.
pub fn exchange_rate(http: &ureq::Agent, from: &str, to: &str) -> Option<f64> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    type RateCache = Mutex<HashMap<(String, String), (f64, Instant)>>;
    static RATES: OnceLock<RateCache> = OnceLock::new();
    let iso = |c: &str| c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase());
    if !iso(from) || !iso(to) {
        return None;
    }
    let key = (from.to_string(), to.to_string());
    let cache = RATES.get_or_init(Default::default);
    if let Some(&(rate, at)) = cache.lock().ok()?.get(&key) {
        if at.elapsed() < Duration::from_secs(12 * 3600) {
            return Some(rate);
        }
    }
    let url = format!("https://api.frankfurter.dev/v1/latest?base={from}&symbols={to}");
    let text = http.get(&url).timeout(Duration::from_secs(10)).call().ok()?.into_string().ok()?;
    let rate = parse_rate(&serde_json::from_str(&text).ok()?, to)?;
    cache.lock().ok()?.insert(key, (rate, Instant::now()));
    Some(rate)
}

/// `{"base":"CNY","rates":{"USD":0.14992}}` → 0.14992.
fn parse_rate(v: &serde_json::Value, to: &str) -> Option<f64> {
    v.get("rates")?.get(to)?.as_f64().filter(|r| r.is_finite() && *r > 0.0)
}

/// Pick the balance bucket to show from a `/user/balance` payload: the largest
/// funded one, since an account can hold an empty `$0.00 USD` bucket next to a
/// `¥135.70 CNY` one. `prefer` (the currency the cost readout is quoted in)
/// breaks ties, so a fully-empty account still reports a figure in the currency
/// the rest of the line uses.
fn pick_balance(v: &serde_json::Value, prefer: &Currency) -> Option<Balance> {
    let parse = |info: &serde_json::Value| -> Option<(f64, Balance)> {
        let amount = info.get("total_balance")?.as_str()?.trim().to_string();
        let value = amount.parse::<f64>().ok()?;
        let code = info.get("currency")?.as_str()?;
        Some((
            value,
            Balance {
                amount,
                currency: Currency::parse(code),
                converted_from: None,
            },
        ))
    };
    v.get("balance_infos")?
        .as_array()?
        .iter()
        .filter_map(parse)
        .max_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                // Tie (e.g. every bucket at 0.00): prefer the price currency.
                .then_with(|| (a.1.currency == *prefer).cmp(&(b.1.currency == *prefer)))
        })
        .map(|(_, bal)| bal)
}

/// The provider's model list URL. Anthropic pages it (20 by default).
fn models_url(cfg: &Config) -> String {
    let base = format!("{}/models", cfg.endpoint_base().trim_end_matches('/'));
    if cfg.chatgpt_sub() {
        // The Codex backend lists the models a client of this version may use.
        let v = std::env::var("HAIKUCODE_CODEX_CLIENT_VERSION").unwrap_or_else(|_| "1.0.0".into());
        return format!("{base}?client_version={v}");
    }
    if cfg.api_kind() == ApiKind::Messages {
        base + "?limit=1000"
    } else {
        base
    }
}

/// Best-effort context window (tokens) for `model` from the provider's
/// `/models` metadata. OpenAI-compatible endpoints usually omit it (DeepSeek
/// returns only id/object/owned_by), but OpenRouter and some others advertise
/// `context_length`. Returns None when the provider doesn't expose it, so the
/// caller can fall back to the built-in table.
pub fn context_window(http: &ureq::Agent, cfg: &Config, model: &str) -> Option<u32> {
    let text = authed(http.get(&models_url(cfg)).timeout(std::time::Duration::from_secs(10)), cfg)
        .call()
        .ok()?
        .into_string()
        .ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let items = v.get("data").cloned().unwrap_or(v);
    let entry = items
        .as_array()?
        .iter()
        .find(|m| m.get("id").and_then(|x| x.as_str()) == Some(model))?;
    // Field name varies across providers; accept the common spellings, plus
    // OpenRouter's nested `top_provider.context_length`.
    let n = ["context_length", "context_window", "max_context_length", "max_context_window", "max_input_tokens"]
        .iter()
        .find_map(|k| entry.get(*k).and_then(|x| x.as_u64()))
        .or_else(|| entry.pointer("/top_provider/context_length").and_then(|x| x.as_u64()))?;
    (n > 0).then_some(n as u32)
}

/// Fetch the provider's available model ids.
pub fn list_models(http: &ureq::Agent, cfg: &Config) -> Result<Vec<String>> {
    let resp = authed(http.get(&models_url(cfg)).timeout(std::time::Duration::from_secs(30)), cfg).call();
    let text = match resp {
        Ok(r) => r.into_string()?,
        Err(ureq::Error::Status(code, r)) => {
            return Err(anyhow!("HTTP {code}: {}", truncate(&r.into_string().unwrap_or_default(), 400)));
        }
        Err(e) => return Err(anyhow!("network error: {e}")),
    };
    let v: serde_json::Value = serde_json::from_str(&text)?;
    Ok(model_ids(&v))
}

/// The model ids in a `/models` payload: OpenAI's `{"data":[{"id"}]}`, a bare
/// array, or the Codex backend's `{"models":[{"slug"}]}` (hidden ones left out).
fn model_ids(v: &serde_json::Value) -> Vec<String> {
    let items = v.get("data").or_else(|| v.get("models")).unwrap_or(v);
    let mut ids: Vec<String> = items
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|m| m.get("visibility").and_then(|x| x.as_str()).is_none_or(|vis| vis != "hide"))
                .filter_map(|m| m.get("id").or_else(|| m.get("slug")).and_then(|x| x.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    ids
}

/// Drop models that can't hold a chat (embeddings, speech, images,
/// moderation, legacy completions) from a provider's list, so the model
/// picker shows only what a coding agent can use. Keeps the list as is when
/// the filter would empty it.
pub fn chat_models(ids: Vec<String>) -> Vec<String> {
    const NOT_CHAT: &[&str] = &[
        "embed", "whisper", "tts", "dall-e", "moderation", "davinci", "babbage", "transcribe", "realtime",
        "audio", "image", "sora", "rerank", "guard", "-search-",
    ];
    let keep: Vec<String> = ids
        .iter()
        .filter(|id| {
            let l = id.to_ascii_lowercase();
            !NOT_CHAT.iter().any(|n| l.contains(n))
        })
        .cloned()
        .collect();
    if keep.is_empty() {
        ids
    } else {
        keep
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    /// The shape DeepSeek actually returns for a China-region account: a funded
    /// CNY bucket next to an empty USD one.
    fn payload() -> serde_json::Value {
        json!({
            "is_available": true,
            "balance_infos": [
                {"currency": "CNY", "total_balance": "135.70", "granted_balance": "0.00", "topped_up_balance": "135.70"},
                {"currency": "USD", "total_balance": "0.00", "granted_balance": "0.00", "topped_up_balance": "0.00"}
            ]
        })
    }

    #[test]
    fn picks_the_funded_bucket_and_keeps_its_currency() {
        let bal = pick_balance(&payload(), &Currency::default()).unwrap();
        assert_eq!(bal.currency.code, "CNY");
        assert_eq!(bal.render(), "¥135.70");
        // The digits are the provider's, not a re-formatted float.
        assert_eq!(bal.amount, "135.70");
    }

    #[test]
    fn a_usd_account_still_reports_usd() {
        let v = json!({"balance_infos": [
            {"currency": "USD", "total_balance": "12.34"},
            {"currency": "CNY", "total_balance": "0.00"}
        ]});
        let bal = pick_balance(&v, &Currency::default()).unwrap();
        assert_eq!(bal.render(), "$12.34");
    }

    #[test]
    fn ties_prefer_the_currency_the_price_list_is_quoted_in() {
        let v = json!({"balance_infos": [
            {"currency": "CNY", "total_balance": "0.00"},
            {"currency": "USD", "total_balance": "0.00"}
        ]});
        // All buckets empty: report one in the currency the cost readout uses,
        // so the status line at least stays in one unit.
        let code = |c: &Currency| pick_balance(&v, c).unwrap().currency.code;
        assert_eq!(code(&Currency::default()), "USD");
        assert_eq!(code(&Currency::parse("cny")), "CNY");
    }

    #[test]
    fn a_payload_without_buckets_is_not_an_error() {
        assert!(pick_balance(&json!({}), &Currency::default()).is_none());
        assert_eq!(parse_rate(&json!({"base":"CNY","rates":{"USD":0.14992}}), "USD"), Some(0.14992));
        assert_eq!(parse_rate(&json!({"rates":{"USD":0}}), "USD"), None);
        assert_eq!(parse_rate(&json!({"message":"not found"}), "USD"), None);
        assert!(pick_balance(&json!({"balance_infos": []}), &Currency::default()).is_none());
        // A currency we have no symbol for still renders with its code.
        let v = json!({"balance_infos": [{"currency": "GBP", "total_balance": "3.50"}]});
        let bal = pick_balance(&v, &Currency::default()).unwrap();
        assert_eq!(bal.render(), "£3.50");
    }
}
