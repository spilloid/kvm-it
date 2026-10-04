# Build environment for the desktop workspace: Linux system libs for btleplug/v4l/egui + a Windows cross target.
FROM docker.io/library/rust:1.99
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libdbus-1-dev libudev-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
        libx11-dev libxcursor-dev libxrandr-dev libxi-dev libgl1-mesa-dev libegl1-mesa-dev libclang-dev clang \
        libv4l-dev gcc-mingw-w64-x86-64 \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-pc-windows-gnu \
    && rustup component add clippy rustfmt
