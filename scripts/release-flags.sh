# Sourced by the mise release tasks (also used by CI): keep absolute paths of
# the build machine — which contain the user name — out of the binary.
# Rust panic locations and C `__FILE__` strings are rewritten to /cargo,
# /rustup and /build. (Cargo's `trim-paths` would do this, but it is not
# stable in Rust 1.98.)
cargo_home=${CARGO_HOME:-$HOME/.cargo}
rustup_home=${RUSTUP_HOME:-$HOME/.rustup}
project_root=$(pwd -P)
RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }--remap-path-prefix=$cargo_home=/cargo --remap-path-prefix=$rustup_home=/rustup --remap-path-prefix=$project_root=/build"
prefix_map="-ffile-prefix-map=$cargo_home=/cargo -ffile-prefix-map=$project_root=/build"
CFLAGS="${CFLAGS:+$CFLAGS }$prefix_map"
CXXFLAGS="${CXXFLAGS:+$CXXFLAGS }$prefix_map"
export RUSTFLAGS CFLAGS CXXFLAGS
