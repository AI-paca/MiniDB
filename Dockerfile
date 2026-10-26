FROM rust:1.90-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY examples ./examples
COPY tests ./tests
COPY run.sh sample.sh ./
RUN cargo test --locked --offline && cargo build --locked --offline --release --bin minidb --examples

FROM debian:bookworm-slim
RUN useradd --uid 10001 --create-home minidb && mkdir /data && chown minidb:minidb /data
COPY --from=build /app/target/release/minidb /usr/local/bin/minidb
COPY --from=build /app/target/release/examples/bench /usr/local/bin/minidb-bench
COPY run.sh sample.sh /usr/local/bin/
ENV MINIDB_BIN=/usr/local/bin/minidb
USER minidb
WORKDIR /data
VOLUME ["/data"]
CMD ["minidb", "/data/defense"]
