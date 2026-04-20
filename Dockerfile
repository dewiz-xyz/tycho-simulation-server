FROM public.ecr.aws/docker/library/rust:1.91.0-bookworm AS builder
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
COPY --from=builder /app/target/release/dsolver-simulator-service /usr/local/bin/dsolver-simulator-service
COPY --from=builder /app/hashflow_supported_tokens.csv /app/hashflow_supported_tokens.csv
COPY --from=builder /app/liquorice_supported_tokens.csv /app/liquorice_supported_tokens.csv
ENV HASHFLOW_FILENAME_CSV=/app/hashflow_supported_tokens.csv
ENV LIQUORICE_FILENAME_CSV=/app/liquorice_supported_tokens.csv

RUN useradd -m appuser
USER appuser

ENTRYPOINT ["dsolver-simulator-service"]
