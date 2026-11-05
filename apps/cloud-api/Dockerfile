# Build from the repository root:  docker build -f apps/cloud-api/Dockerfile .
FROM rust:1.97-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p cloud-api

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 pos
COPY --from=build /src/target/release/cloud-api /usr/local/bin/cloud-api
USER pos
ENV POS_BIND_ADDR=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["cloud-api"]
CMD ["serve"]
