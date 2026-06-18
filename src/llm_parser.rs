//! DeepSeek-backed extraction of a trading-setup card into a TradeSetup,
//! with the deterministic regex parser as fallback.

use crate::parser::{validate_setup, Direction, TakeProfit, TradeSetup};
use serde::Deserialize;

/// Which parser produced the result (for logging/telemetry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseSource {
    Llm,
    RegexFallback,
}

/// JSON shape we ask DeepSeek to return. Kept separate from TradeSetup so the
/// model contract is explicit and lenient (strings tolerated for the enum).
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

impl LlmSetup {
    /// Maps the LLM JSON into a validated TradeSetup, reusing parser validation.
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

/// Extracts the JSON object the model returned (its message content) and maps it.
/// Exposed for unit testing without network.
pub fn parse_llm_content(content: &str) -> anyhow::Result<TradeSetup> {
    // DeepSeek with response_format=json_object returns a JSON object as content.
    let llm: LlmSetup = serde_json::from_str(content.trim())?;
    llm.into_trade_setup()
}

const SYSTEM_PROMPT: &str = "You extract crypto perp trading setups from a pasted card into STRICT JSON. \
Return ONLY a JSON object with keys: coin (string ticker, e.g. \"PENDLE\"), direction (\"LONG\" or \"SHORT\"), \
timeframe (string or null), risk_reward (number or null, the left side of the R:R ratio), confidence (integer 0-10 or null), \
entry (number), stop_loss (number), take_profits (array of {price: number, allocation_pct: number}). \
Strip currency symbols. allocation_pct is the position fraction to close at that TP (e.g. 60), NOT the price-change percent. \
If only one TP and no allocation is given, use 100. Do not invent values that are not in the card.";

/// Calls the DeepSeek chat API and returns the parsed TradeSetup.
pub async fn parse_setup_llm(
    http: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    card_text: &str,
) -> anyhow::Result<TradeSetup> {
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
    parse_llm_content(content)
}

/// Tries the LLM future first; on ANY error, falls back to the regex parser.
/// Returns the setup and which source produced it.
pub async fn parse_with_fallback<F>(
    llm_attempt: F,
    card_text: &str,
) -> Result<(TradeSetup, ParseSource), crate::parser::ParseError>
where
    F: std::future::Future<Output = anyhow::Result<TradeSetup>>,
{
    match llm_attempt.await {
        Ok(setup) => Ok((setup, ParseSource::Llm)),
        Err(error) => {
            tracing::warn!("LLM parse failed, falling back to regex: {error}");
            crate::parser::parse_setup(card_text).map(|setup| (setup, ParseSource::RegexFallback))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_valid_llm_json_to_trade_setup() {
        let content = r#"{"coin":"PENDLE","direction":"LONG","timeframe":"swing","risk_reward":2.8,"confidence":8,"entry":1.40,"stop_loss":1.25,"take_profits":[{"price":1.70,"allocation_pct":60},{"price":2.00,"allocation_pct":40}]}"#;
        let setup = parse_llm_content(content).unwrap();
        assert_eq!(setup.coin, "PENDLE");
        assert_eq!(setup.direction, Direction::Long);
        assert_eq!(setup.entry, 1.40);
        assert_eq!(setup.take_profits.len(), 2);
        assert_eq!(setup.take_profits[0].allocation_pct, 60.0);
    }

    #[test]
    fn rejects_llm_json_with_nonpositive_price() {
        let content = r#"{"coin":"BTC","direction":"LONG","entry":0,"stop_loss":0,"take_profits":[{"price":80000,"allocation_pct":100}]}"#;
        assert!(parse_llm_content(content).is_err());
    }

    #[tokio::test]
    async fn falls_back_to_regex_when_llm_errors() {
        let card = "Trading setup for PENDLE\nDirection\nLONG\nSL\n$1.25\nEntry\n$1.40\nTP1\n$1.70\n100%";
        let failing = async { anyhow::bail!("simulated LLM outage") };
        let (setup, source) = parse_with_fallback(failing, card).await.unwrap();
        assert_eq!(source, ParseSource::RegexFallback);
        assert_eq!(setup.coin, "PENDLE");
    }

    #[tokio::test]
    async fn uses_llm_result_when_it_succeeds() {
        let card = "irrelevant — regex would also work but LLM wins";
        let good = async {
            parse_llm_content(r#"{"coin":"ETH","direction":"SHORT","entry":3000,"stop_loss":3100,"take_profits":[{"price":2800,"allocation_pct":100}]}"#)
        };
        let (setup, source) = parse_with_fallback(good, card).await.unwrap();
        assert_eq!(source, ParseSource::Llm);
        assert_eq!(setup.coin, "ETH");
        assert_eq!(setup.direction, Direction::Short);
    }
}
