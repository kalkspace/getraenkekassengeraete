FROM rust as builder

WORKDIR /src

# https://github.com/tonistiigi/binfmt/issues/122#issuecomment-1359175441
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

RUN apt-get update && apt-get install -y libpcsclite-dev
COPY . /src/

RUN cargo build --release

FROM debian:buster


RUN apt-get update && apt-get install -y tini libpcsclite1 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/getraenkekassengeraete /usr/bin/getraenkekassengeraete

EXPOSE 3030
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/bin/getraenkekassengeraete"]