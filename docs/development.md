# Development

[Back to README](../README.md)

## Tests and sources

From the repository root, run `./build.sh` before the checks below.

```sh
cargo test --manifest-path native/opendrop-rs/Cargo.toml --locked
python3 tests/check_sender.py
python3 tests/check_receiver.py
QT_QPA_PLATFORM=offscreen python3 tests/check_sender_dialog.py
python3 tests/check_install.py
python3 tests/check_radio_context.py
cmake --build native/owl/build --target tests
native/owl/build/tests/tests
```

An optional GitHub Actions build/test template is provided in [`ci/github-actions.yml.example`](../ci/github-actions.yml.example); copy it to `.github/workflows/test.yml` to enable CI.

Tests cover real IPv6 TLS exchanges, client identities, binary plists, chunked responses, same-connection Ask/Upload, compressed/stored archive blocks, Unicode filenames, path containment, rejection, duplicate filenames, Qt process callbacks, and portable installation. They do not replace a real iPhone/hardware test.

Full modified build sources are in `native/`. See [THIRD_PARTY.md](../THIRD_PARTY.md) for upstream commits, licenses and the local protocol/radio changes. GPL-3.0-only applies to this distribution, with third-party notices retained.
