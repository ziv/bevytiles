# Running the demo in the browser

`bevytiles` compiles for `wasm32-unknown-unknown`. On the web the tile source
swaps its thread pool + disk cache for browser `fetch` on Bevy's task pool
(see `src/source/web.rs`); `NetworkConfig::threads` becomes the number of
concurrent tile downloads and `cache_dir` is ignored.

## Quick dev loop

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-server-runner
cargo run --example demo --target wasm32-unknown-unknown
```

`.cargo/config.toml` registers `wasm-server-runner` as the runner; it prints a
`http://127.0.0.1:1334` URL. The canvas is created by the runner's page, so
keyboard focus may need a click.

## Deployable bundle

```sh
cargo install wasm-bindgen-cli --version "$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | tail -1 | cut -d'"' -f2)"
./web/build.sh                     # → web/dist
python3 -m http.server -d web/dist # any static server works
```

`PROFILE=dev ./web/build.sh` builds faster, unoptimized; the default `wasm-release` profile (LTO, stripped) takes several minutes.

## Notes

- The default tile providers (Esri imagery, AWS Terrarium) send permissive
  CORS headers. A custom provider must too, or the browser refuses the fetch.
- `connect_timeout` / `read_timeout` are not applied on the web; the browser
  owns fetch timeouts.
- Use WebGL2-capable browsers; WebGPU works where Bevy's `webgpu` feature is
  enabled (not the default).
