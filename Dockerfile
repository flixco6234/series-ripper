# Series Ripper - web UI + automatic ripping of DVD / Blu-ray / 4K UHD TV
# series, built on MakeMKV's command-line tool (makemkvcon).
#
# Stage 1 compiles makemkvcon from the official Linux tarballs (MakeMKV has
# no apt package), stage 2 compiles the Rust app, stage 3 is the runtime
# image. To move to a newer MakeMKV, bump MAKEMKV_VERSION - the tarballs are
# listed in the "MakeMKV x.y.z for Linux is available" thread at
# forum.makemkv.com (f=3&t=224).

ARG MAKEMKV_VERSION=2.0.0

FROM debian:bookworm-slim AS makemkv
ARG MAKEMKV_VERSION
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        build-essential pkg-config ca-certificates curl \
        libc6-dev libssl-dev libexpat1-dev zlib1g-dev libavcodec-dev libavutil-dev; \
    mkdir -p /build; cd /build; \
    curl -fsSL -o oss.tar.gz "https://www.makemkv.com/download/makemkv-oss-${MAKEMKV_VERSION}.tar.gz"; \
    curl -fsSL -o bin.tar.gz "https://www.makemkv.com/download/makemkv-bin-${MAKEMKV_VERSION}.tar.gz"; \
    tar xzf oss.tar.gz; tar xzf bin.tar.gz; \
    cd "/build/makemkv-oss-${MAKEMKV_VERSION}"; \
    ./configure --disable-gui; \
    make -j"$(nproc)"; \
    make install DESTDIR=/staging; \
    cd "/build/makemkv-bin-${MAKEMKV_VERSION}"; \
    mkdir -p tmp; echo accepted > tmp/eula_accepted; \
    make; \
    make install DESTDIR=/staging; \
    test -x /staging/usr/bin/makemkvcon


FROM rust:1.88-slim-bookworm AS app
WORKDIR /src
# Dependencies first (cached as long as Cargo.toml/Cargo.lock don't change).
COPY Cargo.toml Cargo.lock ./
RUN set -eux; \
    mkdir -p src/bin; \
    echo 'fn main() {}' > src/main.rs; \
    echo 'fn main() {}' > src/bin/mock_makemkvcon.rs; \
    cargo build --release --locked --bin series-ripper; \
    rm -rf src
COPY src src
COPY static static
RUN set -eux; \
    touch src/main.rs; \
    cargo build --release --locked --bin series-ripper; \
    cp target/release/series-ripper /series-ripper


FROM debian:bookworm-slim
ARG MAKEMKV_VERSION

# libavcodec/libavutil: required by libmakemkv. Java: MakeMKV uses it for
# Blu-ray discs with BD-J menus/playlist obfuscation. eject: fallback for
# odd USB drives. lsscsi: lists sr/sg device pairs, handy when setting up
# passthrough.
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        ca-certificates libssl3 libexpat1 zlib1g libavcodec59 libavutil57 \
        openjdk-17-jre-headless eject lsscsi tini; \
    rm -rf /var/lib/apt/lists/*

COPY --from=makemkv /staging/ /
RUN ldconfig && makemkvcon -r info disc:9999 | head -1

COPY --from=app /series-ripper /usr/local/bin/series-ripper
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENV DVDRIPPER_DB=/config/series-ripper.db \
    DVDRIPPER_PORT=8080 \
    PUID=99 \
    PGID=100 \
    UMASK=000

LABEL org.opencontainers.image.title="Series Ripper" \
      org.opencontainers.image.description="Rip DVD / Blu-ray / 4K UHD TV series with MakeMKV and name episodes for Plex/Jellyfin" \
      makemkv.version="${MAKEMKV_VERSION}"

EXPOSE 8080
VOLUME ["/config", "/output"]

HEALTHCHECK --interval=60s --timeout=10s --start-period=30s --retries=3 \
    CMD ["/usr/local/bin/series-ripper", "healthcheck"]

ENTRYPOINT ["/usr/bin/tini", "--", "/entrypoint.sh"]
