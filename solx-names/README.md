# solx-names

A random name generator action for solx-core, backed by the
[`names`](https://crates.io/crates/names) crate. One `wasm32-wasip2`
component, one action.

## Action

`/packages/solx-names/random-name` — no required params.

```jsonc
// params
{ "with_id": true } // optional, default false
```

```jsonc
// result
{ "name": "capable-tiger-9f2c1a06" }
```

`with_id` appends an 8-hex-digit id for extra uniqueness — 4 random bytes,
hex-encoded, the same shape as the first group of a v4 GUID (which carries no
version/variant bits, so it's 32 random bits either way).

## Build & install

```powershell
./build.ps1 -Install
```

```bash
./build.sh --install
```

Either stages `bin/solx-names.wasm` (read by `install.solx`, so it must exist
before installing) and then runs `solx install-package .`.

## Verify

```
solx script --file verify.solx
```

## Uninstall

```
solx script --file uninstall.solx
```

## Tests

```
cargo test
```

Runs `dispatch`'s pure logic on the host target — `guest.rs` (the
wit-bindgen shim) is `#[cfg(target_arch = "wasm32")]` only, so it's skipped
here and exercised for real only once installed and called through `solx`.
