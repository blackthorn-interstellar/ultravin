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

test: build-dev ## Run the Python tests (builds the extension first).
	@uv run -- pytest
	@echo -e "✅ The tests pass! ✨ 🍰 ✨"

check: afu lint typecheck rust test ## Run all checks (format, lint, typecheck, rust, test).

checku: check

data:  ## Import a pinned vPIC dump into vpic/ (usage: make data DUMP=path.zip MONTH=YYYY_MM).
	@cargo run -p ultravin-build --release -- --dump "$(DUMP)" --month "$(MONTH)" --out vpic

install-uv:  # Install uv if not already installed
	@if ! uv --help >/dev/null 2>&1; then \
		echo "Installing uv..."; \
		wget -qO- https://astral.sh/uv/install.sh | sh; \
		echo -e "\033[0;32m ✔️  uv installed \033[0m"; \
	fi

help: ## Show this help message.
	@## https://gist.github.com/prwhite/8168133#gistcomment-1716694
	@echo -e "$$(grep -hE '^\S+:.*##' $(MAKEFILE_LIST) | sed -e 's/:.*##\s*/:/' -e 's/^\(.\+\):\(.*\)/\\x1b[36m\1\\x1b[m:\2/' | column -c2 -t -s :)" | sort
