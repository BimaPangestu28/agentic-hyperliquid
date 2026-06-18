.PHONY: help run test build check fmt clippy equity

help:           ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-10s\033[0m %s\n", $$1, $$2}'

run:            ## Run the bot with debug logging (reads .env)
	RUST_LOG=info,signal_trader=debug cargo run

test:           ## Run the full test suite
	cargo test

build:          ## Build the release binary
	cargo build --release

check:          ## Type-check without building
	cargo check

fmt:            ## Format the code
	cargo fmt

clippy:         ## Lint with clippy
	cargo clippy -- -D warnings

equity:         ## Check how much collateral the bot will see (read-only API call)
	@bash scripts/check-equity.sh
