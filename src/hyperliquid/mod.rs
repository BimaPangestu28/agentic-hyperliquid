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
    async fn asset_meta(&self, coin: &str) -> anyhow::Result<Option<AssetMeta>>;
    async fn set_leverage(&self, coin: &str, leverage: u32) -> anyhow::Result<()>;
    async fn place_entry(&self, order: &EntryOrder) -> anyhow::Result<OrderResult>;
    async fn place_trigger(&self, order: &TriggerOrder) -> anyhow::Result<OrderResult>;
    async fn position_size(&self, coin: &str) -> anyhow::Result<f64>;
    /// Cancels a resting order by its exchange-assigned order id.
    async fn cancel_order(&self, coin: &str, order_id: u64) -> anyhow::Result<()>;
}

// ── Mock (test-only) ──────────────────────────────────────────────────────────

#[cfg(test)]
pub mod mock {
    use super::*;
    use std::sync::Mutex;

    /// Test double for the Exchange trait.
    ///
    /// - `cancels` records every `cancel_order` call as `(coin, order_id)`.
    /// - `simulated_position` overrides `position_size` when `Some`; otherwise
    ///   the size is derived by summing recorded entry orders for the coin.
    #[derive(Default)]
    pub struct MockExchange {
        pub equity: f64,
        pub meta: Option<AssetMeta>,
        pub entries: Mutex<Vec<EntryOrder>>,
        pub triggers: Mutex<Vec<TriggerOrder>>,
        pub leverage_calls: Mutex<Vec<(String, u32)>>,
        pub cancels: Mutex<Vec<(String, u64)>>,
        /// When `Some(value)`, `position_size` returns that value for every coin,
        /// allowing tests to simulate a partial fill without placing real entries.
        pub simulated_position: Mutex<Option<f64>>,
    }

    #[async_trait]
    impl Exchange for MockExchange {
        async fn equity(&self) -> anyhow::Result<f64> {
            Ok(self.equity)
        }

        async fn asset_meta(&self, _coin: &str) -> anyhow::Result<Option<AssetMeta>> {
            Ok(self.meta)
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

        async fn position_size(&self, coin: &str) -> anyhow::Result<f64> {
            // If a simulated position is configured, use it (supports partial-fill testing).
            if let Some(size) = *self.simulated_position.lock().unwrap() {
                return Ok(size);
            }
            Ok(self
                .entries
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.coin.eq_ignore_ascii_case(coin))
                .map(|e| e.size)
                .sum())
        }

        async fn cancel_order(&self, coin: &str, order_id: u64) -> anyhow::Result<()> {
            self.cancels
                .lock()
                .unwrap()
                .push((coin.to_string(), order_id));
            Ok(())
        }
    }

    #[test]
    fn usdc_total_extracts_collateral() {
        let balances = vec![
            hyperliquid_rust_sdk::UserTokenBalance {
                coin: "USDC".into(),
                hold: "0.0".into(),
                total: "399.0".into(),
                entry_ntl: "0.0".into(),
            },
            hyperliquid_rust_sdk::UserTokenBalance {
                coin: "HYPE".into(),
                hold: "0.0".into(),
                total: "1.0".into(),
                entry_ntl: "0.0".into(),
            },
        ];
        assert_eq!(super::usdc_total(&balances).unwrap(), 399.0);
    }

    #[test]
    fn usdc_total_zero_when_absent() {
        let balances: Vec<hyperliquid_rust_sdk::UserTokenBalance> = vec![];
        assert_eq!(super::usdc_total(&balances).unwrap(), 0.0);
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
    BaseUrl, ClientCancelRequest, ClientLimit, ClientOrder, ClientOrderRequest, ClientTrigger,
    ExchangeClient, ExchangeDataStatus, ExchangeResponseStatus, InfoClient, MarketOrderParams,
};

/// Parses an `ExchangeResponseStatus` returned by the SDK's `order` / `market_open`
/// calls and maps it to our domain `OrderResult`.
///
/// - `Resting` → order sitting in the book; `filled: false`, `order_id: Some(oid)`.
/// - `Filled`  → order fully filled; `filled: true`, `avg_price` from `avg_px`.
/// - `Error`   → exchange rejected the order; returns `Err`.
/// - Other statuses (Success, WaitingForFill, WaitingForTrigger) are treated as
///   "submitted but not yet filled" with whatever oid is available (none for those).
fn parse_order_response(status: ExchangeResponseStatus) -> anyhow::Result<OrderResult> {
    match status {
        ExchangeResponseStatus::Err(message) => {
            Err(anyhow::anyhow!("exchange returned error: {message}"))
        }
        ExchangeResponseStatus::Ok(response) => {
            // Pull the first status out of the statuses array (we always send one order at a time).
            let first_status = response
                .data
                .as_ref()
                .and_then(|d| d.statuses.first())
                .cloned();

            match first_status {
                Some(ExchangeDataStatus::Resting(resting)) => Ok(OrderResult {
                    order_id: Some(resting.oid),
                    filled: false,
                    avg_price: None,
                }),
                Some(ExchangeDataStatus::Filled(filled)) => {
                    let avg_price = filled.avg_px.parse::<f64>().ok();
                    Ok(OrderResult {
                        order_id: Some(filled.oid),
                        filled: true,
                        avg_price,
                    })
                }
                Some(ExchangeDataStatus::Error(message)) => {
                    Err(anyhow::anyhow!("order rejected: {message}"))
                }
                // Success / WaitingForFill / WaitingForTrigger / None — order submitted
                // but no oid available from these variants; treat as "not yet filled".
                _ => Ok(OrderResult {
                    order_id: None,
                    filled: false,
                    avg_price: None,
                }),
            }
        }
    }
}

/// Sums the USDC spot balance used as unified-account collateral.
///
/// In Hyperliquid's unified-account mode, USDC from the SPOT clearinghouse
/// backs perp positions. The perp `accountValue` reads 0; the real collateral
/// is the USDC `total` in `user_token_balances`.
///
/// Returns `Ok(0.0)` when no USDC entry exists (no collateral posted).
///
/// # Errors
/// Returns an error if the USDC `total` field cannot be parsed as `f64`.
fn usdc_total(balances: &[hyperliquid_rust_sdk::UserTokenBalance]) -> anyhow::Result<f64> {
    let usdc = balances.iter().find(|b| b.coin.eq_ignore_ascii_case("USDC"));
    match usdc {
        Some(b) => b
            .total
            .parse::<f64>()
            .map_err(|e| anyhow::anyhow!("cannot parse USDC total {:?}: {e}", b.total)),
        None => Ok(0.0), // no USDC balance => no collateral
    }
}

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
    /// When `true`, equity is read from the spot USDC balance (unified-account mode).
    /// When `false`, equity is read from the perp `accountValue` (standard mode).
    unified: bool,
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

        // Derive signing address before wallet is moved into ExchangeClient.
        let signing_address: H160 = wallet.address();

        // Use the master account address for info queries (equity, positions).
        // Agent/API wallets hold no balance; funds live on the master account.
        let query_address: H160 = match &config.account_address {
            Some(addr) => addr
                .parse::<H160>()
                .map_err(|e| anyhow::anyhow!("invalid HYPERLIQUID_ACCOUNT_ADDRESS: {e}"))?,
            None => {
                tracing::warn!(
                    "HYPERLIQUID_ACCOUNT_ADDRESS unset; querying the agent wallet's own address \
                     — equity/positions will be 0 if you use an API/agent wallet"
                );
                signing_address
            }
        };

        let info = InfoClient::new(None, Some(base_url)).await?;
        let exchange =
            ExchangeClient::new(None, wallet, Some(base_url), None, None).await?;

        Ok(Self {
            info,
            exchange,
            address: query_address,
            unified: config.unified_account,
        })
    }
}

#[async_trait]
impl Exchange for HyperliquidExchange {
    /// Returns the account's total margin-inclusive equity (USDC).
    ///
    /// In **unified-account mode** (`HYPERLIQUID_UNIFIED_ACCOUNT=true`), the perp
    /// clearinghouse `accountValue` is always 0 — collateral lives in the SPOT
    /// clearinghouse. This branch reads the spot USDC `total` via
    /// `user_token_balances` instead.
    ///
    /// In **standard mode** the perp `accountValue` is used (original behaviour).
    async fn equity(&self) -> anyhow::Result<f64> {
        if self.unified {
            let balances = self.info.user_token_balances(self.address).await?;
            usdc_total(&balances.balances)
        } else {
            let state = self.info.user_state(self.address).await?;
            state
                .margin_summary
                .account_value
                .parse::<f64>()
                .map_err(|e| anyhow::anyhow!("cannot parse account_value: {e}"))
        }
    }

    /// Returns sizing metadata for `coin`, or `Ok(None)` when the coin is not
    /// listed in the Hyperliquid perp universe.
    ///
    /// **Semantics:**
    /// - `Ok(Some(meta))` — coin found in the perp universe.
    /// - `Ok(None)` — coin not found; caller should skip gracefully (not an error).
    /// - `Err(_)` — network or SDK failure; caller may retry.
    ///
    /// **Note:** The SDK's `meta().universe` does not expose `max_leverage`; this
    /// implementation returns `max_leverage: 0` as a sentinel meaning "unknown".
    /// Callers must treat 0 as "no SDK-enforced cap" and apply their own limits.
    async fn asset_meta(&self, coin: &str) -> anyhow::Result<Option<AssetMeta>> {
        let meta = self.info.meta().await?;
        match meta.universe.iter().find(|a| a.name.eq_ignore_ascii_case(coin)) {
            Some(sdk_asset) => Ok(Some(AssetMeta {
                sz_decimals: sdk_asset.sz_decimals,
                // SDK AssetMeta has no max_leverage field; return 0 (unknown).
                max_leverage: 0,
            })),
            None => Ok(None),
        }
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

    /// Places an entry order.
    ///
    /// - **Market order** (`limit_price: None`): delegates to `ExchangeClient::market_open`
    ///   with 1% slippage, which fetches the current mid-price and sends an IOC limit with
    ///   proper slippage applied — no extreme-price hacks.
    /// - **Limit order** (`limit_price: Some(px)`): sends a GTC limit via `ExchangeClient::order`.
    ///
    /// In both cases the response is parsed and the resting/filled oid is surfaced.
    async fn place_entry(&self, order: &EntryOrder) -> anyhow::Result<OrderResult> {
        let response = match order.limit_price {
            None => {
                // Use the SDK's proper market_open with 1% slippage.
                self.exchange
                    .market_open(MarketOrderParams {
                        asset: &order.coin,
                        is_buy: order.is_buy,
                        sz: order.size,
                        px: None,
                        slippage: Some(0.01),
                        cloid: None,
                        wallet: None,
                    })
                    .await
                    .map_err(|e| anyhow::anyhow!("place_entry (market) failed: {e}"))?
            }
            Some(limit_px) => {
                let request = ClientOrderRequest {
                    asset: order.coin.clone(),
                    is_buy: order.is_buy,
                    reduce_only: false,
                    limit_px,
                    sz: order.size,
                    cloid: None,
                    order_type: ClientOrder::Limit(ClientLimit {
                        tif: "Gtc".to_string(),
                    }),
                };
                self.exchange
                    .order(request, None)
                    .await
                    .map_err(|e| anyhow::anyhow!("place_entry (limit) failed: {e}"))?
            }
        };

        parse_order_response(response)
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

        let response = self
            .exchange
            .order(request, None)
            .await
            .map_err(|e| anyhow::anyhow!("place_trigger failed: {e}"))?;

        parse_order_response(response)
    }

    /// Returns the absolute size of the open position for `coin` (0.0 if flat).
    async fn position_size(&self, coin: &str) -> anyhow::Result<f64> {
        let state = self.info.user_state(self.address).await?;
        match state.asset_positions.iter().find(|p| p.position.coin.eq_ignore_ascii_case(coin)) {
            Some(p) => Ok(p.position.szi.parse::<f64>()
                .map_err(|e| anyhow::anyhow!("cannot parse position szi {:?}: {e}", p.position.szi))?
                .abs()),
            None => Ok(0.0),
        }
    }

    /// Cancels a resting order by its exchange-assigned order id.
    ///
    /// Uses the SDK's `ExchangeClient::cancel` with `ClientCancelRequest { asset, oid }`.
    async fn cancel_order(&self, coin: &str, order_id: u64) -> anyhow::Result<()> {
        self.exchange
            .cancel(
                ClientCancelRequest {
                    asset: coin.to_string(),
                    oid: order_id,
                },
                None,
            )
            .await
            .map_err(|e| anyhow::anyhow!("cancel_order failed: {e}"))?;
        Ok(())
    }
}
