# Contributing to rub

Thanks for your interest in contributing! Here's how to get started.

## Development Setup

```bash
# Prerequisites
# - Rust 1.94.1+ (install via https://rustup.rs)
# - Chrome, Chromium, or Edge browser

# Clone and build
git clone https://github.com/QingChang1204/rub.git
cd rub
cargo build

# Run tests (unit + contract, no browser needed)
cargo test --workspace

# Run integration tests (requires Chrome)
cargo test --workspace -- --ignored
```

## Project Structure

```
crates/
  rub-core/           # Shared types, models, errors
  rub-cdp/            # Chrome DevTools Protocol adapter
  rub-ipc/            # IPC protocol and transport
  rub-daemon/         # Persistent daemon runtime
  rub-cli/            # CLI entry point and command definitions
  rub-test-harness/   # Shared test utilities
fixtures/             # Test fixture HTML files
```

## Making Changes

1. **Fork** the repository
2. **Create a branch** from `main`: `git checkout -b my-feature`
3. **Make your changes** — follow existing code style
4. **Run the quality gate**:
   ```bash
   cargo fmt --check
   cargo clippy -- -D warnings
   cargo test --workspace
   ```
5. **Commit** with a clear message
6. **Open a Pull Request** against `main`

## Code Style

- Rust Edition 2024
- Use `thiserror` for domain errors (`anyhow` is not used)
- Async runtime: `tokio`
- CLI framework: `clap` (derive API)
- Structured logging: `tracing`

## Reporting Bugs

Open an issue with:
- The `rub` command you ran
- Expected vs actual output
- Your OS and Chrome version

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).
