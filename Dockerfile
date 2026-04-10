# Stage 1: Chef — pre-built cargo-chef for dependency layer caching
FROM lukemathwalker/cargo-chef:latest-rust-1.88-alpine AS chef
RUN apk add --no-cache musl-dev
WORKDIR /app

# Stage 2: Prepare — generate recipe from Cargo.toml/Cargo.lock
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY migrations/ migrations/
RUN cargo chef prepare --recipe-path recipe.json

# Stage 3: Cook — build dependencies (cached unless Cargo.toml/lock change)
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json

# Build the binary
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY migrations/ migrations/
RUN cargo build --release --locked --bin multilinguarr

# Stage 4: Runtime — minimal Alpine with ffmpeg
FROM alpine:3.21

ARG VERSION=dev
ARG GIT_SHA=unknown
ARG BUILD_DATE=unknown

LABEL org.opencontainers.image.source=https://github.com/dlepaux/multilinguarr
LABEL org.opencontainers.image.description="Enforces multi-language audio in the *arr media stack"
LABEL org.opencontainers.image.licenses=MIT
LABEL org.opencontainers.image.version=$VERSION
LABEL org.opencontainers.image.revision=$GIT_SHA
LABEL org.opencontainers.image.created=$BUILD_DATE

RUN apk add --no-cache ffmpeg curl

# Non-root user
RUN addgroup -S multilinguarr && adduser -S multilinguarr -G multilinguarr

# Data directory for SQLite database
RUN mkdir -p /data && chown multilinguarr:multilinguarr /data

COPY --from=builder /app/target/release/multilinguarr /usr/local/bin/multilinguarr

USER multilinguarr

EXPOSE 3100

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD curl -sf http://localhost:3100/health || exit 1

ENTRYPOINT ["/usr/local/bin/multilinguarr"]
