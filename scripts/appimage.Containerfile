# Build environment for the Linux AppImage. Ubuntu 22.04 on purpose: the AppImage runs on any distro whose glibc is at
# least the one it was built against, and 22.04's 2.35 also covers Debian 12 and everything newer. (The everyday
# build image, rs.Containerfile, is Debian trixie with glibc 2.41, which would make the AppImage need glibc 2.39.)
FROM docker.io/library/ubuntu:22.04@sha256:08ea48a03a3e78ebc7cd526e6a275053223aadd88bfc09cc49b06d5281525fde
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential pkg-config curl ca-certificates git libdbus-1-dev libudev-dev libxkbcommon-dev \
        libxkbcommon-x11-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev libgl1-mesa-dev \
        libegl1-mesa-dev libclang-dev clang libv4l-dev \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
# Same compiler as scripts/rs.Containerfile (rust:1.99).
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.99.0
