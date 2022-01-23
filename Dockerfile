FROM rust:1.58

RUN mkdir /opt/lovebot
WORKDIR /opt/lovebot
COPY . .

RUN cargo build --release

CMD cargo run --release