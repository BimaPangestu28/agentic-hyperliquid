//! Hyperliquid access behind an `Exchange` trait so orchestration is testable
//! without network. `HyperliquidExchange` is the only network-touching code.

use crate::sizing::AssetMeta;
use async_trait::async_trait;

// ── Public domain types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct EntryOrder {
    pub coin: String,
    pub is_buy: bool,
    pub size: f64,
    /// `None` means market order.
    pub limit_price: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TriggerOrder {
    pub coin: String,
    pub is_buy: bool,
    pub size: f64,
    pub trigger_price: f64,
    pub is_take_profit: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderResult {
    pub order_id: Option<u64>,
    pub filled: bool,
    pub avg_price: Option<f64>,
}

// ── Exchange trait ────────────────────────────────────────────────────────────

#[async_trait]
pub trait Exchange: Send + Sync {
    async fn equity(&self) -> anyhow::Result<f64>;
    async fn asset_meta(&self, coin: &str) -> anyhow::Result<AssetMeta>;
    async fn set_leverage(&self, coin: &str, leverage: u32) -> anyhow::Result<()>;
    async fn place_entry(&self, order: &EntryOrder) -> anyhow::Result<OrderResult>;
    async fn place_trigger(&self, order: &TriggerOrder) -> anyhow::Result<OrderResult>;
    async fn position_size(&self, coin: &str) -> anyhow::Result<f64>;
}

// ── Mock (test-only) ──────────────────────────────────────────────────────────

#[cfg(test)]
pub mod mock {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct MockExchange {
        pub equity: f64,
        pub meta: Option<AssetMeta>,
        pub entries: Mutex<Vec<EntryOrder>>,
        pub triggers: Mutex<Vec<TriggerOrder>>,
        pub leverage_calls: Mutex<Vec<(String, u32)>>,
    }

    #[async_trait]
    impl Exchange for MockExchange {
        async fn equity(&self) -> anyhow::Result<f64> {
            Ok(self.equity)
        }

        async fn asset_meta(&self, _coin: &str) -> anyhow::Result<AssetMeta> {
            self.meta.ok_or_else(|| anyhow::anyhow!("no meta configured"))
        }

        async fn set_leverage(&self, coin: &str, leverage: u32) -> anyhow::Result<()> {
            self.leverage_calls
                .lock()
                .unwrap()
                .push((coin.to_string(), leverage));
            Ok(())
        }

        async fn place_entry(&self, order: &EntryOrder) -> anyhow::Result<OrderResult> {
            self.entries.lock().unwrap().push(order.clone());
            Ok(OrderResult {
                order_id: Some(1),
                filled: order.limit_price.is_none(),
                avg_price: Some(order.limit_price.unwrap_or(1.40)),
            })
        }

        async fn place_trigger(&self, order: &TriggerOrder) -> anyhow::Result<OrderResult> {
            self.triggers.lock().unwrap().push(order.clone());
            Ok(OrderResult {
                order_id: Some(2),
                filled: false,
                avg_price: None,
            })
        }

        async fn position_size(&self, _coin: &str) -> anyhow::Result<f64> {
            Ok(self.entries.lock().unwrap().iter().map(|e| e.size).sum())
        }
    }

    #[tokio::test]
    async fn mock_records_entry_and_trigger_orders() {
        let exchange = MockExchange {
            equity: 5000.0,
            meta: Some(AssetMeta {
                sz_decimals: 1,
                max_leverage: 10,
            }),
            ..Default::default()
        };
        let entry = EntryOrder {
            coin: "PENDLE".into(),
            is_buy: true,
            size: 10.0,
            limit_price: None,
        };
        let result = exchange.place_entry(&entry).await.unwrap();
        assert!(result.filled);
        assert_eq!(exchange.entries.lock().unwrap().len(), 1);
        assert_eq!(exchange.position_size("PENDLE").await.unwrap(), 10.0);
    }
}

// ── Real Hyperliquid implementation ───────────────────────────────────────────

use crate::config::{Config, Network};
use ethers::signers::{LocalWallet, Signer};
use ethers::types::H160;
use hyperliquid_rust_sdk::{
    BaseUrl, ClientLimit, ClientOrder, ClientOrderRequest, ClientTrigger, ExchangeClient,
    InfoClient,
};

/// Live connection to Hyperliquid via the SDK.
///
/// # Deviations from the brief's SDK sketch
///
/// - `meta::AssetMeta` (SDK) only exposes `name` and `sz_decimals`; it has **no**
///   `max_leverage` field at the struct level.  `max_leverage` is available at
///   runtime inside each `PositionData` (per position), but not in the perp
///   metadata returned by `InfoClient::meta()`.  To satisfy the trait we fall
///   back to returning `max_leverage: 0` (unknown) from `asset_meta()`; the
///   caller (`sizing::build_plan`) must guard against this.
///   A real deployment should call `user_state` after opening a position and
///   read `position.max_leverage` if it needs a hard cap.
///
/// - `update_leverage` takes `(leverage: u32, coin: &str, is_cross: bool,
///   wallet: Option<&LocalWallet>)` — the brief had the args in a different
///   order and omitted `wallet`.
///
/// - `ExchangeClient::new` arity is exactly five args in this version:
///   `(Option<Client>, LocalWallet, Option<BaseUrl>, Option<Meta>, Option<H160>)`.
///
/// - The signer crate is `ethers` v2 (not `alloy`); wallet construction uses
///   `str::parse::<LocalWallet>()` which parses a hex private key.
///
/// - `user_state` takes `H160` by value (not reference).
pub struct HyperliquidExchange {
    info: InfoClient,
    exchange: ExchangeClient,
    address: H160,
}

impl HyperliquidExchange {
    /// Connects to Hyperliquid using `config.agent_key` (hex private key).
    ///
    /// # Errors
    /// Returns an error if the private key is invalid or the initial network
    /// handshake fails.
    pub async fn connect(config: &Config) -> anyhow::Result<Self> {
        let base_url = match config.network {
            Network::Testnet => BaseUrl::Testnet,
            Network::Mainnet => BaseUrl::Mainnet,
        };

        // Parse the agent key as a hex private key string.
        let wallet: LocalWallet = config
            .agent_key
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid HYPERLIQUID_AGENT_KEY: {e}"))?;

        let address: H160 = wallet.address();

        let info = InfoClient::new(None, Some(base_url)).await?;
        let exchange =
            ExchangeClient::new(None, wallet, Some(base_url), None, None).await?;

        Ok(Self {
            info,
            exchange,
            address,
        })
    }
}

#[async_trait]
impl Exchange for HyperliquidExchange {
    /// Returns the account's total margin-inclusive equity (USDC).
    async fn equity(&self) -> anyhow::Result<f64> {
        let state = self.info.user_state(self.address).await?;
        let account_value = state
            .margin_summary
            .account_value
            .parse::<f64>()
            .map_err(|e| anyhow::anyhow!("cannot parse account_value: {e}"))?;
        Ok(account_value)
    }

    /// Returns sizing metadata for `coin`.
    ///
    /// **Note:** The SDK's `meta().universe` does not expose `max_leverage`; this
    /// implementation returns `max_leverage: 0` as a sentinel meaning "unknown".
    /// Callers must treat 0 as "no SDK-enforced cap" and apply their own limits.
    async fn asset_meta(&self, coin: &str) -> anyhow::Result<AssetMeta> {
        let meta = self.info.meta().await?;
        let sdk_asset = meta
            .universe
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(coin))
            .ok_or_else(|| anyhow::anyhow!("unknown asset: {coin}"))?;

        Ok(AssetMeta {
            sz_decimals: sdk_asset.sz_decimals,
            // SDK AssetMeta has no max_leverage field; return 0 (unknown).
            max_leverage: 0,
        })
    }

    /// Sets cross-margin leverage for `coin` using the SDK's `update_leverage`.
    ///
    /// The SDK signature is:
    /// `update_leverage(leverage, coin, is_cross, wallet) -> Result<ExchangeResponseStatus>`
    async fn set_leverage(&self, coin: &str, leverage: u32) -> anyhow::Result<()> {
        self.exchange
            .update_leverage(leverage, coin, true, None)
            .await
            .map_err(|e| anyhow::anyhow!("set_leverage failed: {e}"))?;
        Ok(())
    }

    /// Places an entry order (market when `limit_price` is `None`, GTC limit otherwise).
    ///
    /// Market orders are implemented as aggressive IOC limits: buy at `f64::MAX`,
    /// sell at `0.0` — the exchange fills what it can immediately.
    async fn place_entry(&self, order: &EntryOrder) -> anyhow::Result<OrderResult> {
        let (limit_px, tif) = match order.limit_price {
            Some(px) => (px, "Gtc"),
            None => {
                let px = if order.is_buy { f64::MAX } else { 0.0_f64 };
                (px, "Ioc")
            }
        };

        let request = ClientOrderRequest {
            asset: order.coin.clone(),
            is_buy: order.is_buy,
            reduce_only: false,
            limit_px,
            sz: order.size,
            cloid: None,
            order_type: ClientOrder::Limit(ClientLimit {
                tif: tif.to_string(),
            }),
        };

        self.exchange
            .order(request, None)
            .await
            .map_err(|e| anyhow::anyhow!("place_entry failed: {e}"))?;

        Ok(OrderResult {
            order_id: None,
            filled: order.limit_price.is_none(),
            avg_price: order.limit_price,
        })
    }

    /// Places a TP or SL trigger order (reduce-only).
    async fn place_trigger(&self, order: &TriggerOrder) -> anyhow::Result<OrderResult> {
        let tpsl = if order.is_take_profit { "tp" } else { "sl" };

        let request = ClientOrderRequest {
            asset: order.coin.clone(),
            is_buy: order.is_buy,
            reduce_only: true,
            limit_px: order.trigger_price,
            sz: order.size,
            cloid: None,
            order_type: ClientOrder::Trigger(ClientTrigger {
                trigger_px: order.trigger_price,
                is_market: true,
                tpsl: tpsl.to_string(),
            }),
        };

        self.exchange
            .order(request, None)
            .await
            .map_err(|e| anyhow::anyhow!("place_trigger failed: {e}"))?;

        Ok(OrderResult {
            order_id: None,
            filled: false,
            avg_price: None,
        })
    }

    /// Returns the absolute size of the open position for `coin` (0.0 if flat).
    async fn position_size(&self, coin: &str) -> anyhow::Result<f64> {
        let state = self.info.user_state(self.address).await?;
        let size = state
            .asset_positions
            .iter()
            .find(|p| p.position.coin.eq_ignore_ascii_case(coin))
            .map(|p| {
                p.position
                    .szi
                    .parse::<f64>()
                    .unwrap_or(0.0)
                    .abs()
            })
            .unwrap_or(0.0);
        Ok(size)
    }
}
