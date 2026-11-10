# Common tasks.  `make help` lists them.

CARGO ?= cargo
# `make deb V=0.3.1` (or `make version V=0.3.1`) sets a new version first.
VERSION := $(or $(V),$(shell sed -n 's/^version = "\(.*\)"/\1/p' crates/app/Cargo.toml | head -1))
DEB := target/recall_$(VERSION)_amd64.deb
# Settings for `make demo` (test tones instead of real devices).
DEMO_DIR := target/demo

# Development headers the Rust GTK/GStreamer bindings link against.
PKG_MODULES := gtk4 gstreamer-1.0 gstreamer-app-1.0

.PHONY: help deps vendor check build release run run-release test lint fmt deb version docker-test install demo clean

help: ## Show this list
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  make %-12s %s\n", $$1, $$2}'

deps: ## Install build dependencies (asks for sudo password) and vendor/
	scripts/install-build-deps.sh

vendor: ## Download the Vosk library and Vulkan SDK parts into vendor/ (no sudo)
	scripts/fetch-vosk.sh
	scripts/fetch-vulkan-sdk.sh

check: ## Check that Rust and the build dependencies are installed
	@command -v $(CARGO) >/dev/null || { echo "Rust not found: install it from https://rustup.rs, then open a new terminal (or run: . ~/.cargo/env)"; exit 1; }
	@missing=$$(for m in $(PKG_MODULES); do pkg-config --exists $$m || echo $$m; done); \
	  if [ -n "$$missing" ]; then echo "Missing development packages ($$(echo $$missing))."; echo "Run once:  make deps"; exit 1; fi
	@command -v cmake >/dev/null || { echo "cmake not found (whisper.cpp needs it). Run once:  make deps"; exit 1; }
	@[ -f vendor/vosk/libvosk.so ] || scripts/fetch-vosk.sh
	@[ -x vendor/vulkan-sdk/bin/glslc ] || scripts/fetch-vulkan-sdk.sh

build: check ## Debug build
	$(CARGO) build

release: check ## Optimized build: target/release/recall
	$(CARGO) build --release

run: check ## Run the debug build
	$(CARGO) run

run-release: check ## Run the optimized build
	$(CARGO) run --release

test: check ## Run all tests
	$(CARGO) test

lint: check ## Check formatting and clippy
	$(CARGO) fmt --all --check
	$(CARGO) clippy --all-targets -- -D warnings

fmt: ## Format the code
	$(CARGO) fmt --all

deb: check ## Self-contained .deb (bundles GTK/GStreamer/Vosk; runs on Ubuntu 22.04+); V=x.y.z bumps first
	$(if $(V),scripts/bump-version.sh $(V))
	scripts/build-bundle-deb.sh

version: ## Set the version of the crates and the .deb: make version V=x.y.z
	@[ -n "$(V)" ] || { echo "Usage: make version V=x.y.z (now $(VERSION))"; exit 1; }
	scripts/bump-version.sh $(V)

docker-test: deb ## Install the .deb in clean Ubuntu containers and run --self-test
	scripts/docker-test.sh

install: deb ## Build and install the Debian package (asks for sudo password)
	sudo apt install ./$(DEB)

demo: build ## Run with test tones instead of the real microphone/output (separate settings)
	@mkdir -p $(DEMO_DIR)
	@[ -f $(DEMO_DIR)/config.toml ] || printf 'microphone = "test"\nothers = "test"\nhotkey = ""\noutput_dir = "%s"\n' "$(CURDIR)/$(DEMO_DIR)/calls" > $(DEMO_DIR)/config.toml
	target/debug/recall --config-dir $(DEMO_DIR)

clean: ## Delete target/ (all build output, the .deb and demo settings)
	$(CARGO) clean
