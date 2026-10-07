FROM rust:1.93.1-bookworm AS rust-build
WORKDIR /app
RUN rustup target add wasm32-unknown-unknown
COPY Cargo.toml Cargo.lock build.rs ./
COPY src src
COPY migrations migrations
COPY wasm wasm
RUN cargo build --release --locked -p RuxRedock && cargo build --release --locked -p ruxredock-wasm --target wasm32-unknown-unknown
RUN version=$(awk '/^name = "wasm-bindgen"$/{getline;gsub(/"/,"",$3);print $3;exit}' Cargo.lock) && cargo install wasm-bindgen-cli --version "$version" --locked
RUN wasm-bindgen target/wasm32-unknown-unknown/release/ruxredock_wasm.wasm --target web --out-dir /wasm

FROM node:20-bookworm-slim AS frontend-build
WORKDIR /app/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci --include=dev
COPY frontend ./
COPY --from=rust-build /wasm src/wasm
RUN npm run check && npx vite build

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* && useradd --system --uid 10001 ruxredock
WORKDIR /app
COPY --from=rust-build /app/target/release/RuxRedock /usr/local/bin/RuxRedock
COPY --from=frontend-build /app/frontend/dist frontend/dist
RUN mkdir -p .data && chown ruxredock .data
USER ruxredock
ENV BIND_ADDR=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["RuxRedock"]
CMD ["serve"]
