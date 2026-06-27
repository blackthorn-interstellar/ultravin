SHELL := /bin/bash

init: install-uv ## Setup a dev environment for local development.
	uv sync --all-extras
	uv tool install ruff@0.0.287
	@echo -e "\nEnvironment setup! ✨ 🍰 ✨ 🐍 \n"
	@echo -e "The following commands are available to run in the Makefile\n"
	@make -s help

afu: autoformat
af: autoformat  ## Alias for `autoformat`
autoformat:  ## Run the autoformatter.
	@uvx ruff@0.0.287 check --select RUF001,RUF002,RUF003 --fix --isolated .
	@uv run -- ruff check . --fix-only --unsafe-fixes
	@uv run -- ruff format .

lint:  ## Run the code linter.
	@uv run -- ruff check .
	@echo -e "✅ No linting errors - well done! ✨ 🍰 ✨"

typecheck: ## Run the type checker.
	@uv run -- ty check
	@echo -e "✅ No type errors - well done! ✨ 🍰 ✨"

test:  ## Run the tests.
	@uv run -- pytest
	@echo -e "✅ The tests pass! ✨ 🍰 ✨"

check: afu lint typecheck test ## Run all checks.

checku: check

publish:  ## Build and upload the package to PyPI.
	@echo -e "\n\033[0;34m📦 Building and uploading to PyPI...\033[0m\n"
	@rm -rf dist
	@uv run -- python -m build
	@uv run -- twine upload dist/* --repository pypi -u __token__
	@echo "\n\033[0;32m✅ 📦 Package published successfully to pypi! ✨ 🍰 ✨\033[0m\n"

install-uv:  # Install uv if not already installed
	@if ! uv --help >/dev/null 2>&1; then \
		echo "Installing uv..."; \
		wget -qO- https://astral.sh/uv/install.sh | sh; \
		echo -e "\033[0;32m ✔️  uv installed \033[0m"; \
	fi

help: ## Show this help message.
	@## https://gist.github.com/prwhite/8168133#gistcomment-1716694
	@echo -e "$$(grep -hE '^\S+:.*##' $(MAKEFILE_LIST) | sed -e 's/:.*##\s*/:/' -e 's/^\(.\+\):\(.*\)/\\x1b[36m\1\\x1b[m:\2/' | column -c2 -t -s :)" | sort