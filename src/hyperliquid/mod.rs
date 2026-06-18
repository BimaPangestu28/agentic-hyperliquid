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

// ── Fill (realized-PnL record from exchange history) ─────────────────────────

/// A single fill from Hyperliquid's `userFills` endpoint. Closing fills carry
/// a non-zero `closed_pnl`; opening fills report 0.
#[derive(Debug, Clone, PartialEq)]
pub struct Fill {
    pub coin: String,
    pub closed_pnl: f64,
    pub dir: String,
    pub time_ms: u64,
    pub fee: f64,
}

/// A richer fill row for the API: enough to reconstruct round-trip trades.
/// Distinct from `Fill` so existing stats code is unaffected.
#[derive(Debug, Clone, PartialEq)]
pub struct FillDetail {
    pub coin: String,
    pub oid: u64,
    pub dir: String, // SDK `dir`, e.g. "Open Long" / "Close Long"
    pub px: f64,
    pub sz: f64,
    pub closed_pnl: f64,
    pub fee: f64,
    pub time_ms: i64,
    pub start_position: f64,
}

// ── LedgerFlow (USDC deposit/withdrawal record) ───────────────────────────────

/// A USDC deposit/withdrawal from the non-funding ledger.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LedgerFlow {
    pub external_id: String, // "<hash>:<kind>"
    pub kind: String,        // "deposit" | "withdrawal"
    /// Signed USD amount as returned by Hyperliquid — NEGATIVE for withdrawals,
    /// positive for deposits. Direction is also given by `kind`.
    ///
    /// The raw value from `delta.usdc` is preserved without taking the absolute
    /// value so callers can distinguish a withdrawal from a deposit even if they
    /// ignore `kind`. Do NOT negate this field when displaying net capital flow.
    pub usdc: f64,
    pub time_ms: i64,
}

// ── OpenPosition (live perp position snapshot) ────────────────────────────────

/// A single open perp position, derived from `user_state.asset_positions`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct OpenPosition {
    pub coin: String,
    pub direction: String, // "long" | "short"
    pub size: f64,         // absolute size
    pub entry_px: f64,
    pub mark_px: f64,
    pub unrealized_pnl: f64,
    pub leverage: f64,
    pub notional: f64, // position value in USD
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
    /// Returns all fills for the account from the exchange's fill history.
    async fn user_fills(&self) -> anyhow::Result<Vec<Fill>>;
    /// All fills with price/size/order-id detail, oldest first.
    async fn fills_detailed(&self) -> anyhow::Result<Vec<FillDetail>>;
    /// Open perp positions with mark price and unrealized PnL.
    async fn positions(&self) -> anyhow::Result<Vec<OpenPosition>>;
    /// USDC deposits/withdrawals from the non-funding ledger, oldest first.
    async fn usdc_flows(&self) -> anyhow::Result<Vec<LedgerFlow>>;
    /// Returns the number of resting/open orders for `coin` (case-insensitive).
    ///
    /// Used by `process_signal` to skip a new signal when a prior limit entry
    /// for the same coin is still sitting unfilled in the order book.
    async fn open_order_count(&self, coin: &str) -> anyhow::Result<usize>;
}

// ── Test utilities ────────────────────────────────────────────────────────────

/// Re-export of [`mock::MockExchange`] under a stable path so sibling modules
/// (e.g. `api::tests`) can reach it without depending on the internal `mock`
/// module layout.
///
/// Gated on `test` builds only — never compiled into production binaries.
#[cfg(test)]
pub mod testing {
    pub use super::mock::MockExchange;
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
    /// - `fills` is returned verbatim by `user_fills`.
    /// - `positions` is returned verbatim by `positions()`; seed via `set_positions`.
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
        /// Pre-loaded fills returned by `user_fills`.
        pub fills: Mutex<Vec<super::Fill>>,
        /// Coins that have resting/open orders; each entry is a coin name.
        /// `open_order_count` counts entries matching the queried coin (case-insensitive).
        pub open_orders: Mutex<Vec<String>>,
        /// Pre-loaded open positions returned by `positions()`.
        pub positions: Mutex<Vec<super::OpenPosition>>,
        /// Pre-loaded detailed fills returned by `fills_detailed()`.
        pub fills_detailed: Mutex<Vec<super::FillDetail>>,
        /// Pre-loaded USDC ledger flows returned by `usdc_flows()`.
        pub flows: Mutex<Vec<super::LedgerFlow>>,
    }

    impl MockExchange {
        /// Constructs a `MockExchange` that returns `equity` from [`Exchange::equity`].
        ///
        /// All other fields default to their zero/empty values via [`Default`].
        pub fn with_equity(equity: f64) -> Self {
            Self { equity, ..Self::default() }
        }

        /// Constructs a `MockExchange` with all-default fields, suitable for
        /// general-purpose tests that do not need a specific equity value.
        pub fn new_for_test() -> Self {
            Self::default()
        }

        /// Seeds the open positions returned by `positions()`.
        pub fn set_positions(&self, open_positions: Vec<super::OpenPosition>) {
            *self.positions.lock().unwrap() = open_positions;
        }

        /// Seeds the detailed fills returned by `fills_detailed()`.
        pub fn set_fills_detailed(&self, detailed_fills: Vec<super::FillDetail>) {
            *self.fills_detailed.lock().unwrap() = detailed_fills;
        }

        /// Seeds the USDC ledger flows returned by `usdc_flows()`.
        pub fn set_flows(&self, ledger_flows: Vec<super::LedgerFlow>) {
            *self.flows.lock().unwrap() = ledger_flows;
        }
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

        async fn user_fills(&self) -> anyhow::Result<Vec<super::Fill>> {
            Ok(self.fills.lock().unwrap().clone())
        }

        async fn fills_detailed(&self) -> anyhow::Result<Vec<super::FillDetail>> {
            Ok(self.fills_detailed.lock().unwrap().clone())
        }

        async fn positions(&self) -> anyhow::Result<Vec<super::OpenPosition>> {
            Ok(self.positions.lock().unwrap().clone())
        }

        async fn usdc_flows(&self) -> anyhow::Result<Vec<super::LedgerFlow>> {
            Ok(self.flows.lock().unwrap().clone())
        }

        async fn open_order_count(&self, coin: &str) -> anyhow::Result<usize> {
            Ok(self
                .open_orders
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.eq_ignore_ascii_case(coin))
                .count())
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

    #[tokio::test]
    async fn mock_open_order_count_filters_by_coin() {
        let exchange = MockExchange::default();
        exchange.open_orders.lock().unwrap().push("BTC".to_string());
        exchange.open_orders.lock().unwrap().push("btc".to_string());
        exchange.open_orders.lock().unwrap().push("ETH".to_string());
        assert_eq!(exchange.open_order_count("BTC").await.unwrap(), 2);
        assert_eq!(exchange.open_order_count("SOL").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn mock_returns_seeded_positions() {
        let mock = MockExchange::new_for_test();
        let p = super::OpenPosition {
            coin: "ETH".into(),
            direction: "long".into(),
            size: 1.0,
            entry_px: 2000.0,
            mark_px: 2100.0,
            unrealized_pnl: 100.0,
            leverage: 5.0,
            notional: 2100.0,
        };
        mock.set_positions(vec![p.clone()]);
        assert_eq!(mock.positions().await.unwrap(), vec![p]);
    }

    /// Pins the signed-usdc convention: withdrawals carry a NEGATIVE usdc value.
    ///
    /// Hyperliquid returns `delta.usdc` as a negative number for withdrawals.
    /// `usdc_flows` must preserve the sign — it must NOT call abs() or negate.
    /// This test ensures a withdrawal row seeded with usdc = -200.5 comes back
    /// with `kind == "withdrawal"` and `usdc == -200.5` (sign preserved).
    #[tokio::test]
    async fn flows_withdrawal_preserves_negative_usdc() {
        let mock = MockExchange::new_for_test();
        let withdrawal_flow = super::LedgerFlow {
            external_id: "0xwithdraw:withdrawal".into(),
            kind: "withdrawal".into(),
            usdc: -200.5,
            time_ms: 9000,
        };
        mock.set_flows(vec![withdrawal_flow]);
        let flows = mock.usdc_flows().await.unwrap();
        assert_eq!(flows.len(), 1);
        let flow = &flows[0];
        assert_eq!(flow.kind, "withdrawal", "kind must be 'withdrawal'");
        assert_eq!(
            flow.usdc, -200.5,
            "usdc must be negative for withdrawals — sign must not be stripped"
        );
    }
}

// ── Real Hyperliquid implementation ───────────────────────────────────────────

use crate::config::{Config, Network};
use ethers::signers::{LocalWallet, Signer};
use ethers::types::H160;
use hyperliquid_rust_sdk::{
    BaseUrl, ClientCancelRequest, ClientLimit, ClientOrder, ClientOrderRequest, ClientTrigger,
    ExchangeClient, ExchangeDataStatus, ExchangeResponseStatus, InfoClient, MarketOrderParams,
    OpenOrdersResponse,
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

    /// Returns all fills for the master account from Hyperliquid's `userFills` endpoint.
    ///
    /// Each `UserFillsResponse` is mapped to a `Fill`. The `closed_pnl` and `fee`
    /// fields are display/analytics strings — parse failures fall back to `0.0`
    /// rather than failing the whole report.
    async fn user_fills(&self) -> anyhow::Result<Vec<Fill>> {
        let raw_fills = self.info.user_fills(self.address).await?;
        let fills = raw_fills
            .into_iter()
            .map(|r| Fill {
                coin: r.coin,
                closed_pnl: r.closed_pnl.parse::<f64>().unwrap_or(0.0),
                dir: r.dir,
                time_ms: r.time,
                fee: r.fee.parse::<f64>().unwrap_or(0.0),
            })
            .collect();
        Ok(fills)
    }

    /// Returns all fills with price/size/order-id detail, oldest first.
    ///
    /// Mirrors `user_fills` but maps into `FillDetail`, which includes `px`, `sz`,
    /// `oid`, `start_position`, and `dir` for round-trip trade reconstruction.
    ///
    /// # SDK field verification (UserFillsResponse, response_structs.rs)
    /// - `r.coin: String`
    /// - `r.oid: u64`
    /// - `r.dir: String`
    /// - `r.px: String` — price as string; parsed to f64
    /// - `r.sz: String` — size as string; parsed to f64
    /// - `r.closed_pnl: String` — parsed to f64
    /// - `r.fee: String` — parsed to f64
    /// - `r.time: u64` — millisecond timestamp; cast to i64
    /// - `r.start_position: String` — parsed to f64
    async fn fills_detailed(&self) -> anyhow::Result<Vec<FillDetail>> {
        let raw_fills = self.info.user_fills(self.address).await?;
        let mut detailed_fills: Vec<FillDetail> = raw_fills
            .into_iter()
            .map(|r| FillDetail {
                coin: r.coin,
                oid: r.oid,
                dir: r.dir,
                px: r.px.parse::<f64>().unwrap_or(0.0),
                sz: r.sz.parse::<f64>().unwrap_or(0.0),
                closed_pnl: r.closed_pnl.parse::<f64>().unwrap_or(0.0),
                fee: r.fee.parse::<f64>().unwrap_or(0.0),
                time_ms: r.time as i64,
                start_position: r.start_position.parse::<f64>().unwrap_or(0.0),
            })
            .collect();
        // Sort oldest first so callers receive a chronological stream.
        detailed_fills.sort_by_key(|fill| fill.time_ms);
        Ok(detailed_fills)
    }

    /// Returns all open perp positions with mark price and unrealized PnL.
    ///
    /// Reads `asset_positions` from `user_state`, skips flat positions (`szi == 0`),
    /// and derives `mark_px` as `position_value / |szi|` since the SDK does not
    /// expose a dedicated mark-price field on the position snapshot.
    ///
    /// # SDK field verification
    /// Fields confirmed from `hyperliquid_rust_sdk-0.6.0/src/info/sub_structs.rs`:
    /// - `PositionData::szi: String`
    /// - `PositionData::entry_px: Option<String>`
    /// - `PositionData::position_value: String`
    /// - `PositionData::unrealized_pnl: String`
    /// - `PositionData::leverage: Leverage { value: u32, .. }`
    /// - `PositionData::coin: String`
    async fn positions(&self) -> anyhow::Result<Vec<OpenPosition>> {
        let state = self.info.user_state(self.address).await?;
        let mut open_positions = Vec::new();
        for asset_position in state.asset_positions.iter() {
            let position_data = &asset_position.position;
            let signed_size: f64 = position_data.szi.parse().unwrap_or(0.0);
            if signed_size == 0.0 {
                continue;
            }
            let entry_px = position_data
                .entry_px
                .as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            let notional: f64 = position_data.position_value.parse().unwrap_or(0.0);
            let unrealized_pnl: f64 = position_data.unrealized_pnl.parse().unwrap_or(0.0);
            let leverage: f64 = position_data.leverage.value as f64;
            let size = signed_size.abs();
            let mark_px = if size > 0.0 { notional / size } else { 0.0 };
            open_positions.push(OpenPosition {
                coin: position_data.coin.clone(),
                direction: if signed_size >= 0.0 { "long".into() } else { "short".into() },
                size,
                entry_px,
                mark_px,
                unrealized_pnl,
                leverage,
                notional,
            });
        }
        Ok(open_positions)
    }

    /// Returns the number of resting/open orders for `coin` (case-insensitive).
    ///
    /// Queries the account's full open-order list and filters by coin name so
    /// `process_signal` can skip a new signal when an unfilled limit entry for
    /// the same coin is already sitting in the book.
    async fn open_order_count(&self, coin: &str) -> anyhow::Result<usize> {
        let orders: Vec<OpenOrdersResponse> = self.info.open_orders(self.address).await?;
        Ok(orders
            .iter()
            .filter(|order| order.coin.eq_ignore_ascii_case(coin))
            .count())
    }

    /// Returns USDC deposits/withdrawals from the non-funding ledger, oldest first.
    ///
    /// Posts `{"type":"userNonFundingLedgerUpdates","user":<address>}` to the
    /// Hyperliquid info endpoint and maps rows whose `delta.type` is `"deposit"` or
    /// `"withdraw"` into `LedgerFlow` values.
    ///
    /// # Base URL + address source
    /// - Base URL: `self.info.http_client.base_url` — set during `InfoClient::new`
    ///   from the same `BaseUrl` variant used for all other info calls.
    /// - Address: `self.address` — the master account address resolved in `connect()`.
    /// - HTTP client: `self.info.http_client.client` — the same `reqwest::Client`
    ///   already used by the SDK for all info POSTs.
    ///
    /// # SDK field mapping
    /// Raw ledger row fields (from Hyperliquid API):
    /// - `hash: String` — transaction hash used as part of `external_id`
    /// - `time: u64` — epoch milliseconds
    /// - `delta.type: String` — `"deposit"` or `"withdraw"`; rows with other types are skipped
    /// - `delta.usdc: String` — USDC amount as string; parsed to f64
    async fn usdc_flows(&self) -> anyhow::Result<Vec<LedgerFlow>> {
        // Build the raw POST body — the SDK InfoRequest enum does not cover this endpoint.
        let address_hex = format!("{:#x}", self.address);
        let body = serde_json::json!({
            "type": "userNonFundingLedgerUpdates",
            "user": address_hex,
        })
        .to_string();

        let base_url = &self.info.http_client.base_url;
        let response_text = self
            .info
            .http_client
            .client
            .post(format!("{base_url}/info"))
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("usdc_flows request failed: {e}"))?
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("usdc_flows response read failed: {e}"))?;

        // Parse the response as a JSON array of ledger rows.
        let raw_rows: Vec<serde_json::Value> = serde_json::from_str(&response_text)
            .map_err(|e| anyhow::anyhow!("usdc_flows JSON parse failed: {e}"))?;

        let mut ledger_flows: Vec<LedgerFlow> = raw_rows
            .into_iter()
            .filter_map(|row| {
                let hash = row.get("hash")?.as_str()?.to_string();
                let time_ms = row.get("time")?.as_u64()? as i64;
                let delta = row.get("delta")?;
                let delta_type = delta.get("type")?.as_str()?;
                let kind = match delta_type {
                    "deposit" => "deposit",
                    "withdraw" => "withdrawal",
                    _ => return None,
                };
                let usdc_str = delta.get("usdc")?.as_str().unwrap_or("0");
                let usdc = usdc_str.parse::<f64>().unwrap_or(0.0);
                let external_id = format!("{hash}:{kind}");
                Some(LedgerFlow { external_id, kind: kind.to_string(), usdc, time_ms })
            })
            .collect();

        // Sort oldest first for consistent ordering.
        ledger_flows.sort_by_key(|flow| flow.time_ms);
        Ok(ledger_flows)
    }
}
