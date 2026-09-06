SHELL := /bin/sh

CARGO ?= cargo
DOCKER_COMPOSE ?= docker compose
PYTHON ?= python3

.DEFAULT_GOAL := help

.PHONY: help fmt fmt-fix check clippy test build ci deny openapi \
	db-up db-down migrate run-api run-worker docker-build compose-migrate \
	compose-up compose-down

help: ## Show available commands.
	@awk 'BEGIN {FS = ":.*## "; printf "Silicon Remind commands:\n\n"} /^[a-zA-Z0-9_-]+:.*## / {printf "  %-18s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

fmt: ## Check Rust formatting.
	$(CARGO) fmt --all -- --check

fmt-fix: ## Format Rust sources in place.
	$(CARGO) fmt --all

check: ## Type-check every target and feature with the lockfile.
	$(CARGO) check --locked --workspace --all-targets --all-features

clippy: ## Run strict Clippy checks.
	$(CARGO) clippy --locked --workspace --all-targets --all-features -- -D warnings

test: ## Run the complete Rust test suite.
	$(CARGO) test --locked --workspace --all-targets --all-features

build: ## Build all release binaries.
	$(CARGO) build --locked --release --bins

openapi: ## Validate the OpenAPI document (requires openapi-spec-validator).
	$(PYTHON) -m openapi_spec_validator openapi.yaml

deny: ## Check dependency advisories, licenses, bans, and sources.
	$(CARGO) deny check

ci: fmt check clippy test openapi ## Run the same required checks as CI.

db-up: ## Start the local PostgreSQL container.
	$(DOCKER_COMPOSE) up -d postgres

db-down: ## Stop local containers without deleting PostgreSQL data.
	$(DOCKER_COMPOSE) down

migrate: ## Apply migrations using the local Rust binary and .env.
	$(CARGO) run --locked --bin remind-migrate

run-api: ## Run the API locally using .env.
	$(CARGO) run --locked --bin remind-api

run-worker: ## Run the scheduler/delivery worker locally using .env.
	$(CARGO) run --locked --bin remind-worker

docker-build: ## Build the shared production-style image.
	docker build --tag silicon-remind:local .

compose-migrate: ## Apply migrations inside the Compose network.
	$(DOCKER_COMPOSE) --profile tools run --build --rm migrate

compose-up: ## Build and run API plus worker profiles.
	$(DOCKER_COMPOSE) --profile api --profile worker up --build

compose-down: ## Stop Compose services without deleting PostgreSQL data.
	$(DOCKER_COMPOSE) --profile api --profile worker --profile tools down
