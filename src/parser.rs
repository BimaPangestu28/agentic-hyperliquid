//! Parses a free-form "Trading setup" card into a structured `TradeSetup`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Long,
    Short,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TakeProfit {
    pub price: f64,
    pub allocation_pct: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TradeSetup {
    pub coin: String,
    pub direction: Direction,
    pub timeframe: Option<String>,
    pub risk_reward: Option<f64>,
    pub confidence: Option<u8>,
    pub entry: f64,
    pub stop_loss: f64,
    pub take_profits: Vec<TakeProfit>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ParseError {
    #[error("missing required fields: {0}")]
    MissingFields(String),
    #[error("invalid value: {0}")]
    InvalidValue(String),
}

/// Strips `$`, `,`, `+`, `%` and surrounding whitespace, then parses a float.
fn parse_money(token: &str) -> Option<f64> {
    let cleaned: String = token
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    cleaned.parse::<f64>().ok()
}

fn find_value_after<'a>(lines: &'a [&'a str], label: &str) -> Option<&'a str> {
    lines
        .iter()
        .position(|line| line.trim().eq_ignore_ascii_case(label))
        .and_then(|index| lines.get(index + 1))
        .map(|line| line.trim())
}

/// Parses a "Trading setup for X" card. Lines are label/value pairs; price
/// lines look like `$1.40`, allocation lines like `60%`.
pub fn parse_setup(text: &str) -> Result<TradeSetup, ParseError> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();

    let coin = lines
        .iter()
        .find_map(|line| line.strip_prefix("Trading setup for "))
        .map(|c| c.trim().to_string())
        .ok_or_else(|| ParseError::MissingFields("coin".into()))?;

    let direction = match find_value_after(&lines, "Direction").map(str::to_ascii_uppercase).as_deref() {
        Some("LONG") => Direction::Long,
        Some("SHORT") => Direction::Short,
        _ => return Err(ParseError::MissingFields("direction".into())),
    };

    let timeframe = find_value_after(&lines, "Timeframe").map(str::to_string);

    let risk_reward = find_value_after(&lines, "Risk : Reward")
        .and_then(|v| v.split(':').next())
        .and_then(|v| v.trim().parse::<f64>().ok());

    let confidence = find_value_after(&lines, "Confidence")
        .and_then(|v| v.split('/').next())
        .and_then(|v| v.trim().parse::<u8>().ok());

    let stop_loss = find_value_after(&lines, "SL")
        .and_then(parse_money)
        .ok_or_else(|| ParseError::MissingFields("stop_loss".into()))?;

    let entry = find_value_after(&lines, "Entry")
        .and_then(parse_money)
        .ok_or_else(|| ParseError::MissingFields("entry".into()))?;

    // Take-profits: for each TPn label, the next price line is the price; the
    // first subsequent line ending in `%` that is not a +/- price-change is the
    // allocation. We treat the LAST `%` line before the next TP/end as allocation.
    let mut take_profits = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let label = line.trim();
        if !(label.len() >= 3 && label.to_ascii_uppercase().starts_with("TP") && label[2..].chars().all(|c| c.is_ascii_digit())) {
            continue;
        }
        let price = lines.get(index + 1).and_then(|l| parse_money(l))
            .ok_or_else(|| ParseError::InvalidValue(format!("{label} price")))?;
        // Scan following lines until the next TP label or end for an allocation %.
        let mut allocation_pct = 100.0;
        for follow in &lines[index + 1..] {
            let f = follow.trim();
            if f.to_ascii_uppercase().starts_with("TP") && f.len() >= 3 && f[2..].chars().all(|c| c.is_ascii_digit()) {
                break;
            }
            // Allocation lines have no sign; price-change lines start with + or -.
            if f.ends_with('%') && !f.starts_with('+') && !f.starts_with('-') {
                if let Some(value) = parse_money(f) {
                    allocation_pct = value;
                }
            }
        }
        take_profits.push(TakeProfit { price, allocation_pct });
    }

    if take_profits.is_empty() {
        return Err(ParseError::MissingFields("take_profits".into()));
    }

    Ok(TradeSetup { coin, direction, timeframe, risk_reward, confidence, entry, stop_loss, take_profits })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Trading setup for PENDLE
Direction
LONG
Timeframe
swing
Risk : Reward
2.8 : 1
Confidence
8/10
Thesis
Pendle just went net deflationary.
Conservative
Moderate
Aggressive
SL
$1.25
-10.7%
Entry
$1.40
TP1
$1.70
+21.4%
60%
TP2
$2.00
+42.9%
40%";

    #[test]
    fn parses_full_sample_card() {
        let setup = parse_setup(SAMPLE).expect("should parse");
        assert_eq!(setup.coin, "PENDLE");
        assert_eq!(setup.direction, Direction::Long);
        assert_eq!(setup.timeframe.as_deref(), Some("swing"));
        assert_eq!(setup.confidence, Some(8));
        assert_eq!(setup.entry, 1.40);
        assert_eq!(setup.stop_loss, 1.25);
        assert_eq!(setup.take_profits.len(), 2);
        assert_eq!(setup.take_profits[0], TakeProfit { price: 1.70, allocation_pct: 60.0 });
        assert_eq!(setup.take_profits[1], TakeProfit { price: 2.00, allocation_pct: 40.0 });
    }

    #[test]
    fn parses_short_direction() {
        let text = "Trading setup for BTC\nDirection\nSHORT\nSL\n$70000\nEntry\n$68000\nTP1\n$64000\n100%";
        let setup = parse_setup(text).expect("should parse");
        assert_eq!(setup.direction, Direction::Short);
        assert_eq!(setup.take_profits[0].allocation_pct, 100.0);
    }

    #[test]
    fn reports_missing_entry() {
        let text = "Trading setup for BTC\nDirection\nLONG\nSL\n$70000\nTP1\n$80000\n100%";
        let err = parse_setup(text).unwrap_err();
        assert_eq!(err, ParseError::MissingFields("entry".into()));
    }
}
