SHELL := /bin/bash

init: install-uv ## Setup a dev environment for local development.
	uv sync --all-extras
	uv tool install ruff@0.0.287
	@echo -e "\nEnvironment setup! ✨ 🍰 ✨ 🐍 \n"
	@echo -e "The following commands are available to run in the Makefile\n"
	@make -s help

afu: autoformat
af: autoformat  ## Alias for `autoformat`
autoformat:  ## Run the autoformatters (Python + Rust).
	@uvx ruff@0.0.287 check --select RUF001,RUF002,RUF003 --fix --isolated .
	@uv run -- ruff check . --fix-only --unsafe-fixes
	@uv run -- ruff format .
	@cargo fmt --all

lint:  ## Run the Python linter.
	@uv run -- ruff check .
	@echo -e "✅ No linting errors - well done! ✨ 🍰 ✨"

typecheck: ## Run the Python type checker.
	@uv run -- ty check
	@echo -e "✅ No type errors - well done! ✨ 🍰 ✨"

rust:  ## Run Rust fmt-check, clippy, and tests.
	@cargo fmt --all -- --check
	@cargo clippy --workspace --all-targets -- -D warnings
	@cargo test --workspace --exclude ultravin-py
	@echo -e "✅ Rust checks pass! ✨ 🍰 ✨"

build-dev:  ## Build the native extension into the dev venv.
	@uv run -- maturin develop --uv

build-wheel:  ## Build a release wheel into target/wheels/ (local dry-run of the release pipeline).
	@uv run -- maturin build --release --locked --out target/wheels
	@echo -e "✅ wheel(s) in target/wheels/ ✨ 🍰 ✨"

test: build-dev ## Run the Python tests (builds the extension first).
	@uv run -- pytest
	@echo -e "✅ The tests pass! ✨ 🍰 ✨"

check: afu lint typecheck rust test ## Run all checks (format, lint, typecheck, rust, test).

checku: check

data:  ## Import a pinned vPIC dump into vpic/ (usage: make data DUMP=path.zip MONTH=YYYY_MM).
	@cargo run -p ultravin-build --release -- --dump "$(DUMP)" --month "$(MONTH)" --out vpic

download:  ## Download a pinned vPIC dump into downloads/ (usage: make download MONTH=YYYY_MM).
	@mkdir -p downloads
	@curl -fSL "https://vpic.nhtsa.dot.gov/Downloads/vPICList_lite_$(MONTH).plain.zip" -o "downloads/vPICList_lite_$(MONTH).plain.zip"
	@echo "downloaded downloads/vPICList_lite_$(MONTH).plain.zip"

oracle-up:  ## Start the Postgres parity oracle (Docker).
	@bash scripts/oracle.sh up

oracle-load:  ## Load a dump into the oracle (usage: make oracle-load DUMP=path.zip).
	@bash scripts/oracle.sh load "$(DUMP)"

oracle-decode:  ## Decode a VIN via the oracle (usage: make oracle-decode VIN=...).
	@bash scripts/oracle.sh decode "$(VIN)"

oracle-down:  ## Stop and remove the oracle.
	@bash scripts/oracle.sh down

oracle-pool-up:  ## Start the 5-oracle parity pool (Docker).
	@bash scripts/oracle.sh pool-up

oracle-pool-load:  ## Load a dump into all 5 oracles (usage: make oracle-pool-load DUMP=path.zip).
	@bash scripts/oracle.sh pool-load "$(DUMP)"

campaign-start:  ## Launch the detached comprehensive campaign (survives this session; needs the loaded 5-oracle pool).
	@uv run -- python scripts/spawn_campaign.py && sleep 2 && echo "campaign launched (detached). progress: make campaign-status"

campaign-status:  ## Show campaign progress.
	@echo "supervisor pid: $$(cat campaign/supervisor.pid 2>/dev/null || echo 'not running')"; \
	for e in systematic covfuzz; do [ -f campaign/status-$$e.json ] && echo "  $$(cat campaign/status-$$e.json)"; [ -f campaign/DONE-$$e ] && echo "  $$e: DONE"; done; \
	echo "  fails: systematic=$$(wc -l < campaign/fails-systematic.jsonl 2>/dev/null || echo 0) covfuzz=$$(wc -l < campaign/fails-covfuzz.jsonl 2>/dev/null || echo 0)"

campaign-stop:  ## Stop the campaign (supervisor + engines).
	@pkill -f campaign-supervisor.sh 2>/dev/null || true; pkill -f "scripts.parity.campaign" 2>/dev/null || true; rm -f campaign/supervisor.pid; echo "campaign stopped"

install-uv:  # Install uv if not already installed
	@if ! uv --help >/dev/null 2>&1; then \
		echo "Installing uv..."; \
		wget -qO- https://astral.sh/uv/install.sh | sh; \
		echo -e "\033[0;32m ✔️  uv installed \033[0m"; \
	fi

help: ## Show this help message.
	@## https://gist.github.com/prwhite/8168133#gistcomment-1716694
	@echo -e "$$(grep -hE '^\S+:.*##' $(MAKEFILE_LIST) | sed -e 's/:.*##\s*/:/' -e 's/^\(.\+\):\(.*\)/\\x1b[36m\1\\x1b[m:\2/' | column -c2 -t -s :)" | sort
