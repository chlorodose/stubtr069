FROM docker.io/library/rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM docker.io/library/debian:bookworm-slim
# iproute2 and jq are provided for --conn-req-url-script scripts.
RUN apt-get update \
    && apt-get install -y --no-install-recommends iproute2 jq \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --home-dir /data stub \
    && mkdir -p /data && chown stub /data
COPY --from=build /src/target/release/stubtr069 /usr/local/bin/stubtr069
USER stub
WORKDIR /data
ENV STUB_DB=/data/stubtr069.db \
    STUB_DOWNLOAD_DIR=/data/downloads \
    STUB_CONN_REQ_LISTEN=0.0.0.0:7547
VOLUME /data
EXPOSE 7547
ENTRYPOINT ["/usr/local/bin/stubtr069"]
