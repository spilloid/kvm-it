#!/usr/bin/env bash
# Build, test and flash the ESP32-S3 firmware using the pinned ESP-IDF container.
# Usage: scripts/fw.sh {test|build|flash|monitor|flash-monitor|shell|clean} [serial-port]
set -euo pipefail

IDF_IMAGE="${IDF_IMAGE:-docker.io/espressif/idf:v5.5}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${2:-${ESPPORT:-/dev/ttyACM0}}"
RUNTIME="${CONTAINER_RUNTIME:-podman}"

idf_run() { # run a command inside the container with the project mounted
    local extra=()
    if [[ "${NEED_PORT:-0}" == 1 ]]; then
        [[ -c "$PORT" ]] || { echo "error: $PORT not found. Is the board's COM USB-C port plugged in?" >&2; exit 2; }
        extra+=(--device "$PORT" --group-add keep-groups)
    fi
    "$RUNTIME" run --rm $([ -t 0 ] && echo -it) "${extra[@]}" \
        -v "$ROOT:/project:z" -w /project/firmware \
        -e HOME=/tmp -e IDF_TARGET=esp32s3 \
        "$IDF_IMAGE" bash -c "source /opt/esp/idf/export.sh >/dev/null 2>&1 && $*"
}

case "${1:-}" in
test)
    mkdir -p "$ROOT/firmware/build-host"
    "$RUNTIME" run --rm -v "$ROOT:/project:z" -w /project/firmware "$IDF_IMAGE" \
        bash -c 'gcc -std=c11 -Wall -Wextra -Werror -fsanitize=address,undefined \
            -o build-host/test_hid_state test/host/test_hid_state.c main/hid_state.c main/ascii_us.c \
            && build-host/test_hid_state \
            && gcc -std=c11 -Wall -Wextra -Werror -fsanitize=address,undefined \
                -o build-host/test_proto_frame test/host/test_proto_frame.c main/proto_frame.c \
            && build-host/test_proto_frame /project/protocol/vectors.txt \
            && gcc -std=c11 -Wall -Wextra -Werror -fsanitize=address,undefined \
                -o build-host/test_logic test/host/test_logic.c main/proto_frame.c main/proto_dispatch.c \
                main/hid_state.c main/led_pattern.c main/button_logic.c \
            && build-host/test_logic'
    ;;
build)         idf_run "idf.py set-target esp32s3 >/dev/null && idf.py build" ;;
flash)         NEED_PORT=1 idf_run "idf.py -p $PORT flash" ;;
monitor)       NEED_PORT=1 idf_run "idf.py -p $PORT monitor" ;;
flash-monitor) NEED_PORT=1 idf_run "idf.py -p $PORT flash monitor" ;;
shell)         idf_run bash ;;
clean)         rm -rf "$ROOT/firmware/build" "$ROOT/firmware/build-host" "$ROOT/firmware/sdkconfig" ;;
*) sed -n '2,3p' "$0"; exit 1 ;;
esac
