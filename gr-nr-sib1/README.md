# gr-nr-sib1

This directory is a GNU Radio 4 OOT adapter and is not a Cargo workspace
member.

`gr::nr_sib1::Decoder` consumes one
`std::vector<std::complex<float>>` per complete window. That item can come
from a PDU source or a tagged-stream-to-vector block. It calls the stable
`nr-sib1-ffi` ABI and publishes one JSON event array. Raw unbounded streams
must be delimited upstream; the decoder never guesses a 200 ms window.

The block was written against the current GR4 source conventions in the local
upstream checkout: C++23, `gr::Block`, typed ports, `GR_MAKE_REFLECTABLE` and
`GR_REGISTER_BLOCK`. The machine used for development has an upstream source
build but no installed GR4 package, so standalone OOT configuration cannot be
claimed as verified. CMake fails clearly if either `gnuradio4Config.cmake` or
the Rust static library is unavailable. CI therefore does not mark an
uncompiled GR4 adapter green.

```sh
cargo build --release -p nr-sib1-ffi
cmake -S gr-nr-sib1 -B gr-nr-sib1/build \
  -DCMAKE_PREFIX_PATH=/path/to/gnuradio4/install
cmake --build gr-nr-sib1/build
```
