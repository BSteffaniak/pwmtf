# Physics qualification benchmark

This benchmark measures the canonical launch corpus without adding a benchmark framework dependency.

```sh
cargo run -p pwmtf_billiards --release --example qualify_physics -- 1000
```

The executable runs every canonical corpus fixture repeatedly, verifies every result settles, and prints total shots, elapsed time, shots per second, and a checksum accumulator that prevents the optimizer from discarding simulation work. Use the same iteration count and release profile when comparing machines or future physics profiles.

WASM compile qualification remains:

```sh
cargo check -p pwmtf_billiards --target wasm32-unknown-unknown --all-targets
```

Browser/device runtime measurements are still required before final profile pinning because `wasm32-unknown-unknown` binaries cannot execute directly under Cargo.
