#!/usr/bin/env bash
# Run one command at the floor: on the oldest Linux a native artefact of Cavalry runs
# on, glibc 2.28 (ADR 0178; docs/how-to/build-a-native-artefact-at-the-floor.md).
#
#   scripts/floor.sh <command> [argument ...]
#
# The command runs in a container of the image below, AlmaLinux 8 with glibc 2.28, a C
# compiler and Python, which the Python packaging project publishes as `manylinux_2_28`.
# The checkout is there at its own path, and its `target/` is this checkout's
# `target/floor/`: what is built there is linked against glibc 2.28, and nothing of it
# mixes with what the host built. `scripts/package.sh` runs itself through this on
# Linux, so `just package` builds every native artefact at the floor and runs it there.
#
# What the container shares with the host is its kernel. The floor is the C library's:
# a system of the floor with an older kernel than the host's is not shown by this.
#
# The image is named by its digest, so a new one is a change to this file. The two
# digests are those of the tag 2026.11.21-1.
set -euo pipefail

FLOOR="2.28"
case "$(uname -m)" in
    x86_64 | amd64)
        IMAGE="quay.io/pypa/manylinux_2_28_x86_64@sha256:55a36963bc0504cdf004011001c1072306402f0b8b83be5a5eba7068f93eec53" ;;
    aarch64 | arm64)
        IMAGE="quay.io/pypa/manylinux_2_28_aarch64@sha256:126d9eea9d6b1717f3e17b2b3ac51da04ef159a5f935dac36fe92d621dc8f246" ;;
    *)
        echo "floor: no image for a $(uname -m) machine: the floor's images are for x86_64 and aarch64; see docs/how-to/build-a-native-artefact-at-the-floor.md" >&2
        exit 1 ;;
esac
HELP="see docs/how-to/build-a-native-artefact-at-the-floor.md"

if [ "$#" -eq 0 ]; then
    echo "floor: no command: scripts/floor.sh <command> [argument ...] runs it on glibc $FLOOR; $HELP" >&2
    exit 2
fi
if ! command -v docker >/dev/null 2>&1; then
    echo "floor: docker is not installed, and the floor is a container of $IMAGE: install Docker Engine or Docker Desktop and run this again; $HELP" >&2
    exit 1
fi
if ! docker version --format '{{.Server.Version}}' >/dev/null 2>&1; then
    echo "floor: docker is installed and its daemon does not answer: start it (\`sudo systemctl start docker\`, or open Docker Desktop), check that this user may use it, and run this again; $HELP" >&2
    exit 1
fi

root="$(cd "$(dirname "$0")/.." && pwd -P)"
mkdir -p "$root/target/floor"

# The toolchain. On Linux it is the host's own, at its own paths: the same compiler
# the other gates use, and the crates they have already fetched. On another system the
# host's toolchain does not run in the container, so one is installed there once, into
# a volume that later runs find.
arguments=()
if [ "$(uname -s)" = Linux ]; then
    cargo_home="${CARGO_HOME:-$HOME/.cargo}"
    rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"
    if [ ! -x "$cargo_home/bin/cargo" ] || [ ! -d "$rustup_home" ]; then
        echo "floor: no Rust toolchain at $cargo_home and $rustup_home: install it with rustup, as README.md says, and run this again; $HELP" >&2
        exit 1
    fi
    arguments+=(--user "$(id -u):$(id -g)" -v "$cargo_home:$cargo_home" -v "$rustup_home:$rustup_home"
                -e "CARGO_HOME=$cargo_home" -e "RUSTUP_HOME=$rustup_home" -e "CAVALRY_FLOOR_INSTALL=")
else
    cargo_home=/floor-toolchain/cargo
    arguments+=(-v cavalry-floor-toolchain:/floor-toolchain
                -e "CARGO_HOME=$cargo_home" -e "RUSTUP_HOME=/floor-toolchain/rustup" -e "CAVALRY_FLOOR_INSTALL=1")
fi

# /dev/shm of a container is 64 MiB unless it is asked for, and a driver's directory is
# there. HOME is a directory any user of the container may write.
exec docker run --rm --shm-size=1g \
    "${arguments[@]}" \
    -v "$root:$root" -v "$root/target/floor:$root/target" -w "$root" \
    -e HOME=/tmp -e CAVALRY_AT_FLOOR="$FLOOR" -e "CAVALRY_FLOOR_CARGO_BIN=$cargo_home/bin" \
    "$IMAGE" bash -c '
        set -euo pipefail
        # Python 3.12, the one of the image that has pip and venv, ahead of the system one.
        export PATH="$CAVALRY_FLOOR_CARGO_BIN:/opt/python/cp312-cp312/bin:$PATH"
        if [ -n "$CAVALRY_FLOOR_INSTALL" ] && [ ! -x "$CAVALRY_FLOOR_CARGO_BIN/cargo" ]; then
            echo "floor: installing the stable Rust toolchain into the volume cavalry-floor-toolchain, once"
            curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs |
                sh -s -- -y --profile minimal --default-toolchain stable --no-modify-path >/dev/null
        fi
        found=$(ldd --version | sed -n "1s/.* //p")
        if [ "$found" != "$CAVALRY_AT_FLOOR" ]; then
            echo "floor: the image has glibc $found, not $CAVALRY_AT_FLOOR: scripts/floor.sh names the wrong image" >&2
            exit 1
        fi
        echo "floor: glibc $found, $(rustc --version), $(cc --version | head -1), $(python3 --version), kernel $(uname -r)"
        exec "$@"
    ' floor "$@"
