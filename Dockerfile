# ColabBoard を 1 つのコンテナで動かす(同期サーバー + Web クライアント)。
#
#   docker build -t colabboard .
#   docker run -p 8080:8080 -v colabboard-data:/data colabboard
#
# ビルドは 2 段階: Rust でクライアント(WASM)とサーバーを作り、実行用の小さなイメージに
# サーバーのバイナリだけを入れる。クライアントはバイナリに埋め込まれている(`embed` 機能)。

FROM rust:1-bookworm AS build
ARG TRUNK_VERSION=0.22.1
# ブラウザ内 AI で NPU / GPU を使う部品(ONNX Runtime Web、約 30 MB)を入れるか。0 で入れない。
ARG LOCAL_AI_ORT=1
ARG TARGETARCH
RUN rustup target add wasm32-unknown-unknown \
 && case "${TARGETARCH:-amd64}" in \
      amd64) arch=x86_64 ;; \
      arm64) arch=aarch64 ;; \
      *) echo "未対応のアーキテクチャ: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
 && curl -fsSL "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-${arch}-unknown-linux-gnu.tar.gz" \
    | tar -xz -C /usr/local/bin trunk

WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    if [ "$LOCAL_AI_ORT" = 1 ] && [ ! -f crates/board-web/assets/ort/ort.min.mjs ]; then tools/fetch-ort.sh; fi \
 && cd crates/board-web && trunk build --release \
 && cd /src && cargo build -p board-server --release --locked --features embed \
 && cp target/release/board-server /board-server \
 && mkdir /data-dir

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /board-server /usr/local/bin/board-server
# nonroot(uid 65532)が書けるように、空の保存先を持たせておく(名前付きボリュームはこの所有者を引き継ぐ)。
COPY --from=build --chown=65532:65532 /data-dir /data
ENV PORT=8080 \
    DATA_DIR=/data \
    RUST_LOG=info
# 保存先(redb)。ボリュームを割り当てて残す。
VOLUME ["/data"]
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s \
  CMD ["/usr/local/bin/board-server", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/board-server"]
