#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
jobs=${JOBS:-$(nproc)}
# Keep build-machine paths out of distributed executables.
if [[ -z ${CARGO_ENCODED_RUSTFLAGS:-} ]]; then
    read -r -a build_rust_flags <<< "${RUSTFLAGS:-}"
    CARGO_ENCODED_RUSTFLAGS=$(IFS=$'\x1f'; printf '%s' "${build_rust_flags[*]}")
fi
export CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS:+$CARGO_ENCODED_RUSTFLAGS$'\x1f'}--remap-path-prefix=$PWD=/usr/src/airdrop-linux"$'\x1f'"--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/usr/src/cargo"
cargo build --manifest-path native/opendrop-rs/Cargo.toml --locked --release
cmake -S native/owl -B native/owl/build -DCMAKE_BUILD_TYPE=Release -DCMAKE_POLICY_VERSION_MINIMUM=3.5 "-DCMAKE_C_FLAGS=-ffile-prefix-map=$PWD=/usr/src/airdrop-linux"
cmake --build native/owl/build --target owl --parallel "$jobs"
mkdir -p AirDropReceiver/bin
install -m 755 native/opendrop-rs/target/release/luftlift AirDropReceiver/bin/luftlift
install -m 755 native/opendrop-rs/target/release/filin AirDropReceiver/bin/filin
install -m 755 native/owl/build/daemon/owl AirDropReceiver/bin/owl
echo 'Built AirDrop binaries. Run python3 install.py to install the launchers.'
