FROM public.ecr.aws/docker/library/rust:1.85.1-bookworm AS builder
WORKDIR /app

RUN apt-get update -y && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev clang cmake git && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./

COPY . .
RUN cargo build --release


FROM public.ecr.aws/debian/debian:bookworm-slim AS runner
RUN apt-get update -y && apt-get install -y --no-install-recommends \
    ca-certificates openssl && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/tycho-price-service /usr/local/bin/tycho-price-service

RUN useradd -m appuser
USER appuser

ENTRYPOINT ["tycho-price-service"]

