//! What an API key has left, for the providers that say on demand:
//! OpenRouter's key limit (`GET {base}/key`) and the account's credits
//! (`GET {base}/credits`). Plans signed in with an account say theirs in
//! [`super::plans::quota`].

use serde_json::Value;

use super::limits::Window;
use super::{agent, read_limited, LIST_TIMEOUT};
use crate::ai::Kind;

/// Whether RAST can ask this kind's provider what a key has left.
pub fn reports(kind: Kind) -> bool {
    matches!(kind, Kind::OpenRouter)
}

/// The key's windows, asked now (blocking HTTP); `None` for a kind that
/// does not say.
pub fn key_quota(kind: Kind, base_url: &str, key: &str) -> Option<Result<Vec<Window>, String>> {
    match kind {
        Kind::OpenRouter => Some(openrouter(base_url, key)),
        _ => None,
    }
}

fn openrouter(base_url: &str, key: &str) -> Result<Vec<Window>, String> {
    let base = base_url.trim().trim_end_matches('/');
    let get = |path: &str| -> Result<Value, String> {
        let response = agent(LIST_TIMEOUT)
            .get(format!("{base}/{path}"))
            .header("Authorization", format!("Bearer {key}"))
            .header("Accept", "application/json")
            .call()
            .map_err(|e| format!("cannot reach OpenRouter: {e}"))?;
        let status = response.status().as_u16();
        let body = read_limited(response.into_body(), 256 * 1024);
        if !(200..300).contains(&status) {
            return Err(super::refusal(status, &body));
        }
        serde_json::from_str(&body)
            .map_err(|_| "OpenRouter answered with something other than its usage".to_string())
    };
    let mut windows = parse_openrouter_key(&get("key")?);
    // The account's credits (a key may not be allowed to read them).
    if let Ok(credits) = get("credits") {
        windows.extend(parse_openrouter_credits(&credits));
    }
    Ok(windows)
}

/// `{data: {limit, limit_remaining, usage, limit_reset}}`: a window when
/// the key has a spending limit.
pub fn parse_openrouter_key(value: &Value) -> Vec<Window> {
    let data = &value["data"];
    let Some(limit) = data["limit"].as_f64().filter(|l| *l > 0.0) else {
        return Vec::new();
    };
    let left = data["limit_remaining"]
        .as_f64()
        .unwrap_or_else(|| limit - data["usage"].as_f64().unwrap_or(0.0));
    let used = (limit - left).clamp(0.0, limit);
    let name = match data["limit_reset"].as_str().unwrap_or("") {
        "" => "Key limit".to_string(),
        reset => format!("Key limit ({reset})"),
    };
    vec![Window {
        name,
        used_percent: Some(share(used, limit)),
        used: Some(cents(used)),
        limit: Some(cents(limit)),
        ..Default::default()
    }]
}

/// `{data: {total_credits, total_usage}}`: the credits bought and spent.
pub fn parse_openrouter_credits(value: &Value) -> Vec<Window> {
    let data = &value["data"];
    let Some(total) = data["total_credits"].as_f64().filter(|t| *t > 0.0) else {
        return Vec::new();
    };
    let used = data["total_usage"]
        .as_f64()
        .unwrap_or(0.0)
        .clamp(0.0, total);
    vec![Window {
        name: "Credits".into(),
        used_percent: Some(share(used, total)),
        used: Some(cents(used)),
        limit: Some(cents(total)),
        ..Default::default()
    }]
}

fn share(used: f64, of: f64) -> f64 {
    ((used * 100.0 / of).clamp(0.0, 100.0) * 10.0).round() / 10.0
}

/// Dollars as cents (the window's counts are whole numbers).
fn cents(dollars: f64) -> i64 {
    (dollars * 100.0).round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openrouter_limits_and_credits_become_windows() {
        let key = json!({"data": {"limit": 20.0, "limit_remaining": 15.0, "usage": 5.0, "limit_reset": "monthly"}});
        let windows = parse_openrouter_key(&key);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].name, "Key limit (monthly)");
        assert_eq!(windows[0].used_percent, Some(25.0));
        assert_eq!((windows[0].used, windows[0].limit), (Some(500), Some(2000)));
        // A key without a limit says nothing to measure against.
        assert!(parse_openrouter_key(&json!({"data": {"limit": null, "usage": 3.2}})).is_empty());
        let credits = parse_openrouter_credits(
            &json!({"data": {"total_credits": 50.0, "total_usage": 12.5}}),
        );
        assert_eq!(credits[0].name, "Credits");
        assert_eq!(credits[0].used_percent, Some(25.0));
        assert!(parse_openrouter_credits(&json!({"data": {"total_credits": 0}})).is_empty());
    }
}
