set shell := ["bash", "-euo", "pipefail", "-c"]

# List operations without building or preparing inputs.
default:
    @just --list

check:
    python3 -B -m unittest discover -s scripts -p test_library.py
    cargo fmt -- --check
    rustfmt --edition 2024 --check src/screen.rs qualification/screen.rs qualification/browser/transport.rs qualification/browser/fixture.rs qualification/browser/receiver_probe.rs
    rustfmt --edition 2024 --check qualification/library.rs qualification/owned-library.rs
    cargo clippy --locked --all-targets -- -D warnings
    cargo test --locked

provider-check:
    cargo test --locked --test provider -- --ignored

screen-check:
    python3 scripts/check-screen.py --domain "$NDS_SCREEN_DOMAIN" --provider-root "$NDS_SCREEN_PROVIDER_ROOT" --legacy-provider "$NDS_SCREEN_LEGACY_PROVIDER" --output "$NDS_SCREEN_OUTPUT"

browser-check:
    python3 scripts/check-browser.py --domain "$NDS_SCREEN_DOMAIN" --provider-root "$NDS_SCREEN_PROVIDER_ROOT" --output "$NDS_BROWSER_OUTPUT" --wasm-bindgen "$NDS_BROWSER_WASM_BINDGEN" --playwright-module "$NDS_BROWSER_PLAYWRIGHT" --browser "$NDS_BROWSER_EXECUTABLE"

library-prepare destination config:
    python3 scripts/prepare-library-provider.py --destination "{{ destination }}" --config "{{ config }}"

# Invoke only in an admitted native Linux consumer workdir with checked inputs.
owned-screen-check:
    cargo test --locked --features owned-screen --lib
    python3 scripts/owned-x11.py -- cargo test --locked --features owned-screen --lib actual_receiver_age_pause_idle_pressure_and_expiry -- --ignored --nocapture
    python3 scripts/owned-x11.py -- cargo test --locked --features owned-screen --lib actual_builtin_host_retains_capture_until_cancel_or_expiry_drains -- --ignored --nocapture
    python3 scripts/owned-x11.py -- cargo test --locked --features owned-screen --lib actual_builtin_host_keeps_original_peer_and_lease_after_foreign_connection -- --ignored --nocapture

# The checks the former CI library ran from .github/nddev-ci.toml; CI calls them through
# `just test` on a pull request and `just verify` on dev.
test:
    cargo clippy --locked --workspace --all-targets -- -D warnings
    cargo test --locked --workspace

ci-extras:
    cargo audit --deny warnings
    rustfmt --edition 2024 --check src/screen.rs qualification/screen.rs qualification/browser/transport.rs qualification/browser/fixture.rs qualification/browser/receiver_probe.rs qualification/library.rs qualification/owned-library.rs
    cargo test --locked --features owned-screen --lib

ci-policy:
    cargo deny --locked --all-features check licenses sources

ci-build:
    cargo build --locked --workspace --release

verify:
    just check
    just test
    just ci-policy
    just ci-extras
    just ci-build
