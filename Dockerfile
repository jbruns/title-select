# syntax=docker/dockerfile:1
# The Service image: one static binary, the workbench's bundle with the licence texts its About page shows, the
# pinned clone lists that Selection reads, and the pinned chdman that CHD builds run, on distroless cc (the C library
# chdman links), run as the nonroot user. It needs no writable path but the store volume, so it runs with a read-only
# root filesystem.

FROM rust:1.99.1-alpine@sha256:b1d5dd5a22a09404d3e798c0cf051b689077360374eb505fa5bb93c479693d87 AS toolchain
ARG TARGETARCH
RUN apk add --no-cache musl-dev
# cargo-leptos builds the workbench; it runs the wasm-bindgen it finds on PATH, which must match Cargo.lock's, and the
# wasm-opt it finds there, or else downloads one of its own unchecked. That wasm-opt is binaryen's release build: it
# links mimalloc, where Alpine's package uses musl's allocator, under which its threads spend minutes contending (#198).
# cargo-chef compiles the dependencies apart from the sources, so a source change reuses them.
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) arch=x86_64; \
        leptos_sha=149b88535fd8f44321da0d667fecc445c15af35f62931b2091687445297cc9e0; \
        bindgen_sha=82d12bb940e2d4e72e0d5605387fc1b8ca179044e012b620f0ce4e7440e8320e; \
        chef_sha=aca691abfbfbbe00d482e0ed2249eec3091b65e96a0ce92947fb5b254d48b16d; \
        binaryen_sha=50b9fa62b9abea752da92ec57e0c555fee578760cd237c40107957715d2976ba ;; \
      arm64) arch=aarch64; \
        leptos_sha=834d34b7bc837ed4bfdc5a26428f7c7b249550a5aecd5e6220b76b702008ab12; \
        bindgen_sha=2ed4351c35dd9440308bbb02767d47ea278efe851a52465300f3c94f5b6c2a87; \
        chef_sha=cd59b90fce5fa84c4189648fec33404ec5491791eebdb812df5e7fca8336408d; \
        binaryen_sha=81d46b86b10876ab615eec67e09fcc5615115a7b189cfe3d466725ee36c46ac2 ;; \
      *) echo "unsupported architecture $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    leptos="cargo-leptos-$arch-unknown-linux-musl"; \
    bindgen="wasm-bindgen-0.2.129-$arch-unknown-linux-musl"; \
    chef="cargo-chef-$arch-unknown-linux-musl"; \
    binaryen="binaryen-version_129-$arch-linux"; \
    wget -q "https://github.com/leptos-rs/cargo-leptos/releases/download/v0.3.11/$leptos.tar.gz"; \
    wget -q "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/0.2.129/$bindgen.tar.gz"; \
    wget -q "https://github.com/LukeMathWalker/cargo-chef/releases/download/v0.1.78/$chef.tar.xz"; \
    wget -q "https://github.com/WebAssembly/binaryen/releases/download/version_129/$binaryen.tar.gz"; \
    printf '%s  %s\n' "$leptos_sha" "$leptos.tar.gz" "$bindgen_sha" "$bindgen.tar.gz" "$chef_sha" "$chef.tar.xz" \
      "$binaryen_sha" "$binaryen.tar.gz" \
      | sha256sum -c -; \
    tar xzf "$leptos.tar.gz"; tar xzf "$bindgen.tar.gz"; tar xJf "$chef.tar.xz"; tar xzf "$binaryen.tar.gz"; \
    install "$leptos/cargo-leptos" "$bindgen/wasm-bindgen" "$chef/cargo-chef" binaryen-version_129/bin/wasm-opt \
      /usr/local/cargo/bin/; \
    rm -r "$leptos"* "$bindgen"* "$chef"* "$binaryen.tar.gz" binaryen-version_129; \
    wasm-opt --version; \
    rustup target add wasm32-unknown-unknown
WORKDIR /src

# The recipe: the workspace's manifests and lockfile, with every source stripped out. It changes only when they do.
FROM toolchain AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# The server and the workbench build in stages of their own, so BuildKit runs them at the same time. Each runs the half
# of cargo-leptos's build its flag selects, so neither repeats a command cargo-leptos would run. A full build makes the
# workbench's bundle first only so that a server could embed it, and this server doesn't.
#
# Each stage's dependencies compile in a layer keyed on the recipe alone. The cook matches its build's package,
# target, target directory, profile and features, and the output name leptos compiles in, so the build reuses what it
# compiled. The fetch adds the crates for other platforms that cargo-leptos's metadata query downloads. target/ is
# removed once the build's output is out of it, so the layer cached for the build stays small.
FROM toolchain AS server
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --recipe-path recipe.json --locked --package title-select --bin title-select --no-default-features \
      --release \
    && cargo fetch --locked
COPY . .
RUN cargo leptos build --release --cargo-locked --server-only \
    && cp target/release/title-select /title-select \
    && rm -r target
# The store volume's mount point, owned by the nonroot user in the final stage.
RUN mkdir /store

FROM toolchain AS front
COPY --from=planner /src/recipe.json recipe.json
RUN LEPTOS_OUTPUT_NAME=title-select cargo chef cook --recipe-path recipe.json --locked --package title-select-ui \
      --no-default-features --target wasm32-unknown-unknown --target-dir target/front --profile wasm-release \
    && cargo fetch --locked
COPY . .
# cargo-leptos logs which wasm-opt it runs, and with what arguments, only at -vv. The build fails unless it ran the
# binaryen installed above with the arguments Cargo.toml sets: a renamed setting would otherwise fall back to -Oz, and
# a missing binary to a download, without a word.
RUN set -o pipefail \
    && cargo leptos build -vv --release --precompress --cargo-locked --frontend-only 2>&1 | tee /tmp/leptos.log \
    && grep -q 'using wasm-opt .*/usr/local/cargo/bin/wasm-opt' /tmp/leptos.log \
    && grep 'running wasm-opt' /tmp/leptos.log | grep -q -- ' -O1 ' \
    && rm /tmp/leptos.log \
    && cp -r target/site /ui \
    && mkdir -p /ui/licences && cp LICENSE /ui/licences/title-select.txt \
    && rm -r target

# The clone lists, metadata, MIAs and RetroAchievements data Selection reads (ADR 0015), at the pinned commit, checked
# against the pinned archive sha256.
FROM debian:12-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3 AS clone-lists
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY third_party/clone-lists third_party/clone-lists
COPY tools/clone-lists tools/clone-lists
RUN tools/clone-lists/fetch /opt/clone-lists
# The curated list of arcade games the arcade selector offers (ADR 0027), at the pinned commit.
COPY third_party/curated-list third_party/curated-list
COPY tools/curated-list tools/curated-list
RUN tools/curated-list/fetch /opt/curated-list

# chdman, for CHD builds, from a pinned MAME release: only chdman is built, not the emulator. MAME's tools build links
# SDL, which chdman never calls: SDL's headers are there to compile against, but a stub stands in for its library, and
# the C++ runtime is linked in, so the binary needs only the C library the runtime image has.
FROM debian:12-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3 AS chdman-build
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl g++ make python3 libsdl2-dev \
    && rm -rf /var/lib/apt/lists/*
# renovate: datasource=github-releases depName=mamedev/mame
ARG MAME_VERSION=mame0289
WORKDIR /mame
RUN curl -fsSL "https://github.com/mamedev/mame/archive/refs/tags/$MAME_VERSION.tar.gz" | tar xz --strip-components=1
COPY tools/chdman/sdl-stub.c /stub/
RUN gcc -O2 -c /stub/sdl-stub.c -o /stub/sdl-stub.o && ar rcs /stub/libSDL2.a /stub/sdl-stub.o
RUN make -j"$(nproc)" SUBTARGET=tiny TOOLS=1 EMULATOR=0 IGNORE_GIT=1 OPTIMIZE=2 SYMBOLS=0 NO_X11=1 NO_OPENGL=1 \
      LDOPTS="-L/stub -static-libstdc++ -static-libgcc" generate build/projects/sdl/mametiny/gmake-linux/Makefile \
    && make -j"$(nproc)" -C build/projects/sdl/mametiny/gmake-linux config=release chdman \
    && mkdir /out && cp chdman /out/chdman && cat COPYING docs/legal/GPL-2.0 > /out/mame.txt \
    && /out/chdman | grep -q "manager 0\.${MAME_VERSION#mame0} " \
    && test -z "$(ldd /out/chdman | grep -v -e linux-vdso -e libc.so -e libm.so -e ld-linux)"

# chdman alone, for CI's tests to export.
FROM scratch AS chdman
COPY --from=chdman-build /out/chdman /chdman

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=server /title-select /title-select
COPY --from=front /ui /ui
COPY --from=server --chown=65532:65532 /store /store
COPY --from=clone-lists /opt/clone-lists /opt/clone-lists
COPY --from=clone-lists /opt/curated-list /opt/curated-list
# Retool's licence, which the selection engine ported from it carries, and the clone lists' licence, for the
# workbench's About page.
COPY third_party/retool/LICENSE /ui/licences/retool.txt
COPY --from=clone-lists /opt/clone-lists/LICENSE /ui/licences/retool-clonelists-metadata.txt
COPY --from=clone-lists /opt/curated-list/LICENSE /ui/licences/curated-mame-arcade-romset.txt
COPY --from=chdman-build /out/chdman /usr/local/bin/chdman
COPY --from=chdman-build /out/mame.txt /ui/licences/mame.txt
ENV TITLE_SELECT_STORE=/store \
    TITLE_SELECT_LISTEN=0.0.0.0:8080 \
    TITLE_SELECT_UI=/ui \
    TITLE_SELECT_CLONE_LISTS=/opt/clone-lists \
    TITLE_SELECT_CURATED_LIST=/opt/curated-list \
    TITLE_SELECT_CHDMAN=/usr/local/bin/chdman
VOLUME /store
EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/title-select"]
CMD ["serve"]
