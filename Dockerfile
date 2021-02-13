FROM rust as builder

WORKDIR /src

RUN apt-get update && apt-get install -y libpcsclite-dev
COPY . /src/

RUN cargo build --release

FROM debian:buster

RUN apt-get update && apt-get install -y libpcsclite1 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/getraenkekassengeraete /usr/bin/getraenkekassengeraete

EXPOSE 3030

CMD ["/usr/bin/getraenkekassengeraete"]