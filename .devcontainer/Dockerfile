# ColabBoard の開発用コンテナ(VS Code Dev Containers / GitHub Codespaces)。
# Rust(ネイティブ + wasm32)、trunk、Node.js(tools/ のスクリプト用)をまとめて入れる。
# trunk の配布バイナリは glibc 2.39 以上を要求するので、bookworm(2.36)ではなく trixie を使う。
FROM mcr.microsoft.com/devcontainers/rust:1-trixie

ARG TRUNK_VERSION=0.22.1
ARG NODE_MAJOR=22
ARG TARGETARCH

# CI と同じ: wasm ターゲットと rustfmt / clippy。
RUN rustup target add wasm32-unknown-unknown \
 && rustup component add rustfmt clippy \
 && chmod -R g+w "${RUSTUP_HOME}" "${CARGO_HOME}"

# trunk(本番用 Dockerfile と同じ版)。wasm-bindgen / wasm-opt は初回の trunk build で自動取得される。
RUN case "${TARGETARCH:-amd64}" in \
      amd64) arch=x86_64 ;; \
      arm64) arch=aarch64 ;; \
      *) echo "未対応のアーキテクチャ: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
 && curl -fsSL "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-${arch}-unknown-linux-gnu.tar.gz" \
    | tar -xz -C /usr/local/bin trunk \
 && trunk --version

# Node.js(tools/bench-web.mjs と tools/perf-gif の Playwright 用)。nodejs.org の配布物から最新の ${NODE_MAJOR}.x を入れる。
RUN case "${TARGETARCH:-amd64}" in \
      amd64) arch=x64 ;; \
      arm64) arch=arm64 ;; \
      *) echo "未対応のアーキテクチャ: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
 && base="https://nodejs.org/dist/latest-v${NODE_MAJOR}.x" \
 && file=$(curl -fsSL "${base}/SHASUMS256.txt" | grep -o "node-v[0-9.]*-linux-${arch}\.tar\.xz" | head -n1) \
 && curl -fsSLO "${base}/${file}" \
 && curl -fsSL "${base}/SHASUMS256.txt" | grep " ${file}\$" | sha256sum -c - \
 && tar -xJf "${file}" -C /usr/local --strip-components=1 --no-same-owner \
    --exclude=CHANGELOG.md --exclude=LICENSE --exclude=README.md \
 && rm "${file}" \
 && node --version && npm --version
