# Build Rust inside a temporary stage; ship only the executable.
FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY telegram/worker.py ./telegram/worker.py
RUN cargo build --release --locked

# Prepare Telegram dependencies separately from the runtime image.
FROM debian:bookworm-slim AS telegram-deps
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates python3 python3-venv \
    && rm -rf /var/lib/apt/lists/*
COPY telegram/requirements.txt /tmp/telegram-requirements.txt
RUN python3 -m venv /opt/moviettd-python \
    && /opt/moviettd-python/bin/python -m pip install --no-cache-dir -r /tmp/telegram-requirements.txt

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates python3 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 moviettd \
    && useradd --system --uid 10001 --gid moviettd --no-create-home --shell /usr/sbin/nologin moviettd \
    && install -d -o moviettd -g moviettd /data /media
COPY --from=telegram-deps /opt/moviettd-python /opt/moviettd-python
COPY --from=builder --chmod=0755 /build/target/release/moviettd-server /usr/local/bin/moviettd-server
USER 10001:10001
WORKDIR /data
ENV MOVIETTD_PYTHON=/opt/moviettd-python/bin/python \
    MOVIETTD_BIND=0.0.0.0:18766 \
    MOVIETTD_DATA_DIR=/data \
    MOVIETTD_LIBRARY_ROOT=/media
EXPOSE 18766
STOPSIGNAL SIGINT
ENTRYPOINT ["/usr/local/bin/moviettd-server"]
