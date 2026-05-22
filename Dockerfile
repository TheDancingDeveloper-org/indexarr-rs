# =============================================================================
# Stage 1: Build Vue 3 UI
# =============================================================================
FROM node:22-alpine AS ui-builder

WORKDIR /ui
COPY ui/package.json ui/package-lock.json* ./
RUN npm install

COPY ui/ ./
RUN npm run build

# =============================================================================
# Stage 2: Build Rust binary
# =============================================================================
# Always build on the host platform to avoid QEMU for Rust compilation.
# Cross-compilation to arm64 is done via the aarch64-linux-gnu toolchain.
FROM --platform=$BUILDPLATFORM rust:1-bookworm AS rust-builder

ARG GIT_AUTH_TOKEN=""
ARG PLUGIN_PASSWORD=""
# Set by Docker buildx: amd64 or arm64
ARG TARGETARCH

WORKDIR /build

RUN printf '[registries.forgejo]\nindex = "sparse+https://repo.indexarr.net/api/packages/indexarr/cargo/"\ncredential-provider = "cargo:token"\n\n[registry]\ndefault = "forgejo"\n' > $CARGO_HOME/config.toml && \
    TOKEN="${GIT_AUTH_TOKEN:-$PLUGIN_PASSWORD}" && \
    printf '[registries.forgejo]\ntoken = "Bearer %s"\n' "$TOKEN" > $CARGO_HOME/credentials.toml

# Install the aarch64 cross-compilation toolchain when targeting arm64
RUN if [ "$TARGETARCH" = "arm64" ]; then \
      apt-get update && apt-get install -y --no-install-recommends \
        gcc-aarch64-linux-gnu g++-aarch64-linux-gnu libc6-dev-arm64-cross cmake && \
      rustup target add aarch64-unknown-linux-gnu; \
    fi

# Cache dependencies by building a dummy project first
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs && \
    if [ "$TARGETARCH" = "arm64" ]; then \
      CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
        cargo build --release --target aarch64-unknown-linux-gnu --features vendored-ssl 2>/dev/null || true; \
    else \
      cargo build --release 2>/dev/null || true; \
    fi

# Build the real binary
COPY src/ src/
RUN touch src/main.rs && \
    if [ "$TARGETARCH" = "arm64" ]; then \
      CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
        cargo build --release --target aarch64-unknown-linux-gnu --features vendored-ssl && \
      cp target/aarch64-unknown-linux-gnu/release/indexarr target/release/indexarr; \
    else \
      cargo build --release; \
    fi

# =============================================================================
# Stage 3: Runtime image (minimal)
# =============================================================================
FROM debian:bookworm-slim

# Copy CA bundle from builder to avoid apt-get, which can fail with GPG
# signature errors inside isolated plugin Docker daemons.
COPY --from=rust-builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

WORKDIR /app

# Copy Rust binary
COPY --from=rust-builder /build/target/release/indexarr /app/indexarr

# Copy Vue UI build
COPY --from=ui-builder /ui/dist /app/ui/dist

# Copy classifier rules if present
COPY classifier.yml* /app/

# Data directory
RUN mkdir -p /app/data

ENV INDEXARR_HOST=0.0.0.0
ENV INDEXARR_PORT=8080
ENV INDEXARR_DATA_DIR=/app/data

EXPOSE 8080
EXPOSE 6881-6884/udp
EXPOSE 6890
EXPOSE 6895/udp

ENTRYPOINT ["/app/indexarr"]
CMD ["--all"]
