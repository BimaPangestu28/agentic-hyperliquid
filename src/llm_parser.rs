//! DeepSeek-backed extraction of trading-setup cards into TradeSetups,
//! with the deterministic regex parser as fallback.
//!
//! The LLM is asked to return a `{"setups":[...]}` array so that one message
//! can yield multiple signals. The regex `parser::parse_setup` remains the
//! single-setup fallback for the original "Trading setup for X" card format.

use crate::parser::{validate_setup, Direction, TakeProfit, TradeSetup};
use serde::Deserialize;

/// Which parser produced the result (for logging/telemetry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseSource {
    Llm,
    RegexFallback,
}

/// JSON shape we ask DeepSeek to return for each individual setup. Kept
/// separate from `TradeSetup` so the model contract is explicit and lenient
/// (strings tolerated for the enum).
#[derive(Debug, Deserialize)]
struct LlmSetup {
    coin: String,
    direction: String,
    #[serde(default)]
    timeframe: Option<String>,
    #[serde(default)]
    risk_reward: Option<f64>,
    #[serde(default)]
    confidence: Option<u8>,
    entry: f64,
    stop_loss: f64,
    take_profits: Vec<LlmTakeProfit>,
}

#[derive(Debug, Deserialize)]
struct LlmTakeProfit {
    price: f64,
    #[serde(default = "default_allocation")]
    allocation_pct: f64,
}

fn default_allocation() -> f64 { 100.0 }

/// Top-level wrapper for the `{"setups":[...]}` array the LLM returns.
#[derive(Debug, Deserialize)]
struct LlmSetupList {
    #[serde(default)]
    setups: Vec<LlmSetup>,
}

impl LlmSetup {
    /// Maps the LLM JSON into a validated `TradeSetup`, reusing parser validation.
    fn into_trade_setup(self) -> anyhow::Result<TradeSetup> {
        let direction = match self.direction.trim().to_ascii_uppercase().as_str() {
            "LONG" => Direction::Long,
            "SHORT" => Direction::Short,
            other => anyhow::bail!("unknown direction from LLM: {other}"),
        };
        let setup = TradeSetup {
            coin: self.coin.trim().to_string(),
            direction,
            timeframe: self.timeframe,
            risk_reward: self.risk_reward,
            confidence: self.confidence,
            entry: self.entry,
            stop_loss: self.stop_loss,
            take_profits: self.take_profits.into_iter()
                .map(|tp| TakeProfit { price: tp.price, allocation_pct: tp.allocation_pct })
                .collect(),
        };
        validate_setup(&setup).map_err(|e| anyhow::anyhow!("LLM setup failed validation: {e}"))?;
        Ok(setup)
    }
}

/// Parses the LLM JSON object `{"setups":[...]}` into validated `TradeSetup`s.
///
/// Invalid individual setups are skipped with a warning; returns `Err` only
/// when NONE of the setups are valid (including the case of an empty array).
pub fn parse_llm_content_multi(content: &str) -> anyhow::Result<Vec<TradeSetup>> {
    let list: LlmSetupList = serde_json::from_str(content.trim())?;
    let mut setups = Vec::new();
    for raw in list.setups {
        match raw.into_trade_setup() {
            Ok(setup) => setups.push(setup),
            Err(e) => tracing::warn!("skipping invalid LLM setup: {e}"),
        }
    }
    if setups.is_empty() {
        anyhow::bail!("LLM returned no valid setups");
    }
    Ok(setups)
}

const SYSTEM_PROMPT: &str = "You extract crypto perp trading setups from a pasted message into STRICT JSON. \
Return ONLY a JSON object: {\"setups\": [ {coin, direction, timeframe, risk_reward, confidence, entry, stop_loss, take_profits:[{price, allocation_pct}]}, ... ]}. \
Extract EVERY distinct trade signal in the message into its own array element. \
Use the ticker symbol for coin (e.g. BITCOIN/$BTC → \"BTC\", SOLANA/$SOL → \"SOL\", HYPERLIQUID/$HYPE → \"HYPE\"). \
direction is \"LONG\" or \"SHORT\". Strip currency symbols and thousands separators ($64,000 → 64000). \
allocation_pct is the fraction of the position to close at that TP. \
If allocations are NOT given and there are multiple TPs, DISTRIBUTE EQUALLY so they sum to 100 (e.g. 2 TPs → 50 and 50; 3 TPs → 34, 33, 33). \
If a single TP with no allocation, use 100. \
timeframe is a string or null. risk_reward is a number or null. confidence is an integer 0-10 or null. \
Do not invent values that are not in the message. \
If the message contains no trade setups, return {\"setups\": []}.";

/// Calls the DeepSeek chat API and returns the parsed `Vec<TradeSetup>`.
///
/// The LLM is instructed to return a `{"setups":[...]}` array so multiple
/// signals in one message are all captured.
pub async fn parse_setups_llm(
    http: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    card_text: &str,
) -> anyhow::Result<Vec<TradeSetup>> {
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": card_text}
        ],
        "response_format": {"type": "json_object"},
        "temperature": 0,
        "stream": false
    });
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let response = http.post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        anyhow::bail!("DeepSeek API error {status}: {text}");
    }
    let value: serde_json::Value = response.json().await?;
    let content = value["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("DeepSeek response missing message content"))?;
    parse_llm_content_multi(content)
}

/// Builds the OpenAI-compatible chat request body for a vision (image) parse.
/// `image_data_url` is a `data:image/...;base64,...` string.
pub fn build_vision_body(model: &str, image_data_url: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": [
                {"type": "text", "text": "Extract all trading setups visible in this image as the specified JSON."},
                {"type": "image_url", "image_url": {"url": image_data_url}}
            ]}
        ],
        "response_format": {"type": "json_object"},
        "temperature": 0,
        "stream": false
    })
}

/// Sends an image to an OpenAI-compatible vision model and returns parsed setups.
///
/// @param http - The HTTP client to use for the request
/// @param base_url - Base URL for the OpenAI-compatible API (e.g. `https://api.openai.com/v1`)
/// @param api_key - OpenAI API key for bearer auth
/// @param vision_model - Vision model identifier (e.g. `gpt-4o-mini`)
/// @param image_data_url - A `data:image/...;base64,...` string with the encoded image
/// @returns Parsed `TradeSetup` list on success
/// @throws anyhow::Error - When the API returns an error or the response is malformed
pub async fn parse_setups_llm_image(
    http: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    vision_model: &str,
    image_data_url: &str,
) -> anyhow::Result<Vec<crate::parser::TradeSetup>> {
    let body = build_vision_body(vision_model, image_data_url);
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let response = http.post(&url).bearer_auth(api_key).json(&body).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        anyhow::bail!("OpenAI vision API error {status}: {text}");
    }
    let value: serde_json::Value = response.json().await?;
    let content = value["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("OpenAI vision response missing message content"))?;
    parse_llm_content_multi(content)
}

/// Tries the LLM (multi) future first; on ANY error, falls back to the regex
/// parser (single setup wrapped in a `Vec`).
///
/// Returns the setups and which source produced them.
pub async fn parse_with_fallback<F>(
    llm_attempt: F,
    card_text: &str,
) -> Result<(Vec<TradeSetup>, ParseSource), crate::parser::ParseError>
where
    F: std::future::Future<Output = anyhow::Result<Vec<TradeSetup>>>,
{
    match llm_attempt.await {
        Ok(setups) => Ok((setups, ParseSource::Llm)),
        Err(error) => {
            tracing::warn!("LLM parse failed, falling back to regex: {error}");
            crate::parser::parse_setup(card_text).map(|s| (vec![s], ParseSource::RegexFallback))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_setups_from_array() {
        let content = r#"{"setups":[
          {"coin":"BTC","direction":"LONG","entry":64000,"stop_loss":62500,"take_profits":[{"price":65800,"allocation_pct":50},{"price":67000,"allocation_pct":50}]},
          {"coin":"SOL","direction":"LONG","entry":71,"stop_loss":68.5,"take_profits":[{"price":74.5,"allocation_pct":50},{"price":76.3,"allocation_pct":50}]},
          {"coin":"HYPE","direction":"LONG","entry":70.5,"stop_loss":66.5,"take_profits":[{"price":75.5,"allocation_pct":50},{"price":76.7,"allocation_pct":50}]}
        ]}"#;
        let setups = parse_llm_content_multi(content).unwrap();
        assert_eq!(setups.len(), 3);
        assert_eq!(setups[0].coin, "BTC");
        assert_eq!(setups[2].coin, "HYPE");
    }

    #[test]
    fn skips_invalid_setup_keeps_valid() {
        let content = r#"{"setups":[
          {"coin":"BTC","direction":"LONG","entry":0,"stop_loss":62500,"take_profits":[{"price":65800,"allocation_pct":100}]},
          {"coin":"SOL","direction":"LONG","entry":71,"stop_loss":68.5,"take_profits":[{"price":74.5,"allocation_pct":100}]}
        ]}"#;
        let setups = parse_llm_content_multi(content).unwrap();
        assert_eq!(setups.len(), 1);
        assert_eq!(setups[0].coin, "SOL");
    }

    #[test]
    fn empty_setups_is_error() {
        assert!(parse_llm_content_multi(r#"{"setups":[]}"#).is_err());
    }

    #[tokio::test]
    async fn falls_back_to_regex_single() {
        let card = "Trading setup for PENDLE\nDirection\nLONG\nSL\n$1.25\nEntry\n$1.40\nTP1\n$1.70\n100%";
        let failing = async { anyhow::bail!("simulated outage") };
        let (setups, source) = parse_with_fallback(failing, card).await.unwrap();
        assert_eq!(source, ParseSource::RegexFallback);
        assert_eq!(setups.len(), 1);
        assert_eq!(setups[0].coin, "PENDLE");
    }

    #[tokio::test]
    async fn uses_llm_array_when_it_succeeds() {
        let good = async { parse_llm_content_multi(r#"{"setups":[{"coin":"ETH","direction":"SHORT","entry":3000,"stop_loss":3100,"take_profits":[{"price":2800,"allocation_pct":100}]}]}"#) };
        let (setups, source) = parse_with_fallback(good, "x").await.unwrap();
        assert_eq!(source, ParseSource::Llm);
        assert_eq!(setups.len(), 1);
        assert_eq!(setups[0].coin, "ETH");
    }

    #[test]
    fn vision_body_includes_image_and_model() {
        let body = build_vision_body("gpt-4o-mini", "data:image/png;base64,AAAA");
        assert_eq!(body["model"], "gpt-4o-mini");
        let serialized = body.to_string();
        assert!(serialized.contains("image_url"));
        assert!(serialized.contains("data:image/png;base64,AAAA"));
        assert!(serialized.contains("json_object"));
    }
}
