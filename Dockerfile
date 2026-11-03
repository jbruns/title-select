# syntax=docker/dockerfile:1
# The Service image: one static binary, the workbench's bundle with the licence texts its About page shows, and the
# patched Retool that Selection runs, on distroless Python, run as the nonroot user. It needs no writable path but the store volume, so it runs with a
# read-only root filesystem.

FROM rust:1.99.1-alpine@sha256:b1d5dd5a22a09404d3e798c0cf051b689077360374eb505fa5bb93c479693d87 AS build
ARG TARGETARCH
RUN apk add --no-cache musl-dev binaryen
# cargo-leptos builds the workbench; it runs the wasm-bindgen it finds on PATH, which must match Cargo.lock's.
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) arch=x86_64; \
        leptos_sha=149b88535fd8f44321da0d667fecc445c15af35f62931b2091687445297cc9e0; \
        bindgen_sha=82d12bb940e2d4e72e0d5605387fc1b8ca179044e012b620f0ce4e7440e8320e ;; \
      arm64) arch=aarch64; \
        leptos_sha=834d34b7bc837ed4bfdc5a26428f7c7b249550a5aecd5e6220b76b702008ab12; \
        bindgen_sha=2ed4351c35dd9440308bbb02767d47ea278efe851a52465300f3c94f5b6c2a87 ;; \
      *) echo "unsupported architecture $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    leptos="cargo-leptos-$arch-unknown-linux-musl"; \
    bindgen="wasm-bindgen-0.2.129-$arch-unknown-linux-musl"; \
    wget -q "https://github.com/leptos-rs/cargo-leptos/releases/download/v0.3.11/$leptos.tar.gz"; \
    wget -q "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/0.2.129/$bindgen.tar.gz"; \
    printf '%s  %s\n' "$leptos_sha" "$leptos.tar.gz" "$bindgen_sha" "$bindgen.tar.gz" | sha256sum -c -; \
    tar xzf "$leptos.tar.gz"; tar xzf "$bindgen.tar.gz"; \
    install "$leptos/cargo-leptos" "$bindgen/wasm-bindgen" /usr/local/cargo/bin/; \
    rm -r "$leptos"* "$bindgen"*; \
    rustup target add wasm32-unknown-unknown
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo leptos build --release --precompress --cargo-locked \
    && cp target/release/title-select /title-select \
    && cp -r target/site /ui \
    && mkdir -p /ui/licences && cp LICENSE /ui/licences/title-select.txt
RUN mkdir /store

# Retool for Selection (ADR 0001): its runtime files, patched, with the pinned clone lists and a venv from the
# hash-locked requirements. The builder's Python is Debian 12's, the same 3.11 the runtime image runs.
FROM debian:12-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3 AS retool
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git python3 python3-venv \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY third_party/retool third_party/retool
COPY tools/retool tools/retool
RUN python3 -m venv /opt/retool/venv \
    && /opt/retool/venv/bin/pip install --no-cache-dir --no-deps --only-binary :all: --require-hashes \
      -r third_party/retool/requirements.txt \
    && python3 tools/retool/build.py build --dest /opt/retool --runtime-only --no-pristine \
    && rm -r /opt/retool/cache

FROM gcr.io/distroless/python3-debian12:nonroot@sha256:7d1042ce588ab97019fe95c24ffca7bc5a82ccdac572511d5e09bda4435c89c5
COPY --from=build /title-select /title-select
COPY --from=build /ui /ui
COPY --from=build --chown=65532:65532 /store /store
COPY --from=retool /opt/retool /opt/retool
# The vendored Retool's licence and the fetched clone lists' licence, for the workbench's About page.
COPY --from=retool /opt/retool/retool/LICENSE /ui/licences/retool.txt
COPY --from=retool /opt/retool/retool/clonelists-LICENSE /ui/licences/retool-clonelists-metadata.txt
ENV TITLE_SELECT_STORE=/store \
    TITLE_SELECT_LISTEN=0.0.0.0:8080 \
    TITLE_SELECT_UI=/ui \
    TITLE_SELECT_RETOOL=/opt/retool
VOLUME /store
EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/title-select"]
CMD ["serve"]
