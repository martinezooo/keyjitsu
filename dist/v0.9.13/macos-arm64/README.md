# Keyjitsu v0.9.13 macOS ARM64 build

Prebuilt macOS app bundle from Keyjitsu v0.9.13.

Source code commit: `49a5d364b4b47b078d26a24fa8917526585ba8f0`
Remote branch documentation head at build time: `a2b16e479553001638f0bd0bdaa9d5d57872a069`

Verification before packaging:
- `cargo fmt --check`
- `cargo clippy --all-targets --locked -- -D warnings`
- `cargo test --locked`: 125 passed, 0 failed, 1 ignored
- `scripts/bundle.sh`: passed

The ZIP is split only because the current GitHub connector cannot upload the 4.6 MB binary in one request.

Reassemble it on macOS:

```sh
cd dist/v0.9.13/macos-arm64
./assemble.sh
```

Output:
`keyjitsu-v0.9.13-macos-arm64.zip`

Expected SHA-256:
`b6810718c32526c9a5e518c5dbb87f56a41334aec9e11f3dd04c13319d8e9b6e`
