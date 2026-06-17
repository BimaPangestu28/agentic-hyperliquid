//! In-memory store holding a parsed setup + computed plan between the
//! confirmation message and the user's button press. Keyed by message id.

use crate::parser::TradeSetup;
use crate::sizing::{AssetMeta, ExecutionPlan, RiskProfile};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct PendingTrade {
    pub setup: TradeSetup,
    pub equity: f64,
    pub asset_meta: AssetMeta,
    pub profile: RiskProfile,
    pub plan: ExecutionPlan,
}

pub struct PendingStore {
    inner: Mutex<HashMap<i64, PendingTrade>>,
}

impl PendingStore {
    pub fn new() -> Self {
        Self { inner: Mutex::new(HashMap::new()) }
    }

    pub fn insert(&self, key: i64, trade: PendingTrade) {
        self.inner.lock().unwrap().insert(key, trade);
    }

    pub fn get(&self, key: i64) -> Option<PendingTrade> {
        self.inner.lock().unwrap().get(&key).cloned()
    }

    pub fn remove(&self, key: i64) -> Option<PendingTrade> {
        self.inner.lock().unwrap().remove(&key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Direction, TakeProfit};
    use crate::sizing::BracketLeg;

    fn sample_trade() -> PendingTrade {
        let setup = TradeSetup {
            coin: "PENDLE".into(),
            direction: Direction::Long,
            timeframe: None,
            risk_reward: None,
            confidence: None,
            entry: 1.40,
            stop_loss: 1.25,
            take_profits: vec![TakeProfit { price: 1.70, allocation_pct: 100.0 }],
        };
        let plan = ExecutionPlan {
            coin: "PENDLE".into(),
            direction: Direction::Long,
            size: 100.0,
            entry: 1.40,
            leverage: 3,
            notional: 140.0,
            margin: 46.6,
            risk_amount: 100.0,
            liquidation_price: 0.93,
            stop_loss: BracketLeg { price: 1.25, size: 100.0 },
            take_profits: vec![BracketLeg { price: 1.70, size: 100.0 }],
            warnings: vec![],
        };
        PendingTrade { setup, equity: 10_000.0, asset_meta: AssetMeta { sz_decimals: 1, max_leverage: 10 }, profile: RiskProfile::Moderate, plan }
    }

    #[test]
    fn insert_get_remove_roundtrip() {
        let store = PendingStore::new();
        store.insert(7, sample_trade());
        assert_eq!(store.get(7).unwrap().plan.size, 100.0);
        assert!(store.remove(7).is_some());
        assert!(store.get(7).is_none());
    }
}
