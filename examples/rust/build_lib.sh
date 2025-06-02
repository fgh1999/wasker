cargo clean
cargo build --release --target wasm32-wasip1

rm -f ./rust.o ./rust.ll
wasker -o rust.o ./target/wasm32-wasip1/release/rust.wasm
ar rcs librust.a ./rust.o
rm -f ./rust.ll

rm -f rust.wat
wasm2wat ./target/wasm32-wasip1/release/rust.wasm > rust.wat
