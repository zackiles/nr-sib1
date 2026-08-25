# nr-sib1

An open-source 5G NR SIB1 decoder, written in Rust from IQ samples to a
CRC-valid broadcast identity.

To the best of our knowledge, this is the only currently available,
fully open-source SIB1 decoder written in Rust. Getting from raw IQ through
PSS/SSS, PBCH polar decoding, Type0-PDCCH, DCI, PDSCH rate recovery, LDPC
combining and ASN.1 was difficult work. If it saves you time, GitHub
[Sponsors](https://github.com/sponsors/zackiles) or donations are deeply
appreciated <3.

## Scope

The decoder supports licensed terrestrial FR1 NR downlink IQ at 15 and 30 kHz
SSB/PBCH spacing, Type0-PDCCH Tables 13-0 through 13-6 where applicable,
PDSCH mapping types A and B, direct and interleaved VRB-to-PRB mapping, every
SI-RNTI MCS a UE may decode, and every SIB1 redundancy version. It emits sync,
MIB, CRC-valid SIB1 and structured failure events.

It does not claim every FR1 cell. Shared-spectrum n46/n96/n102 requires
Tables 13-1A/13-4A and discovery-burst indexing and is explicitly refused.
n104 is above common 6 GHz SDR front-end ceilings. NTN, LTE and FR2 are not
decoded. The crate embeds 3,036 E.212 MCC/MNC assignments and automatically
adds the assigned country and operator name to each PLMN returned by a
CRC-valid SIB1. A PCI or frequency never identifies an operator.

`plan`, `Guard` and `RATES` are receiver policy for finite direct-conversion
captures. Their passband margins, DC clearance, rate ladder and memory budget
are not 3GPP requirements.

## Use it

### Rust core

```rust
use nr_sib1::{Config, Event, decode};
use num_complex::Complex32;

let samples: Vec<Complex32> = acquire_complete_window();
let events: Vec<Event> = decode(&samples, &config);
```

The included example reads SigMF `ci32_le` or `cf32_le` IQ and defaults to the
public n3 fixture:

```sh
cargo run -p nr-sib1 --example decode
cargo run -p nr-sib1 --example decode -- /path/to/sigmf-directory
```

### FutureSDR

`nr-sib1-futuresdr::Decoder` buffers a finite `Complex32` stream until EOS,
then publishes each `Event` as a typed `Pmt::Any` on its `events` message
port. The default window is unbounded; `Decoder::with_limit` makes the memory
limit explicit and reports overflow instead of silently truncating.

```sh
cargo +nightly-2026-08-15 run -p nr-sib1-futuresdr --features futuresdr --example decode_sigmf
```

The workspace toolchain stays stable and the core and FFI crates build on it.
FutureSDR 0.8.0 and futuredsp 0.8.0 need unstable features, so this adapter is
behind the `futuresdr` feature and built by a pinned-nightly CI job. The pin is
deliberate: those features move under a floating `nightly`, and when they do it
fails as dozens of errors inside a dependency rather than as anything
recognisable.

### C, C++ and GNU Radio 4

`nr-sib1-ffi` provides `libnr_sib1_ffi` and
`crates/nr-sib1-ffi/include/nr_sib1.h`. It accepts interleaved `f32` IQ plus a
JSON `Config`, returns an allocated JSON array of `Event` values, and provides
the matching free function. Invalid pointers, lengths, UTF-8 and JSON receive
stable status codes; Rust panics are caught at the ABI boundary.

`gr-nr-sib1/` is intentionally outside the Cargo workspace. Its GNU Radio 4
block consumes one complete tagged/PDU window (`vector<complex<float>>`) and
publishes the event array as JSON. Build the Rust static library first, then
configure CMake against an installed/current GR4 package:

```sh
cargo build --release -p nr-sib1-ffi
cmake -S gr-nr-sib1 -B gr-nr-sib1/build
cmake --build gr-nr-sib1/build
```

GR4 evolves quickly. The block follows the current `gr::Block`, `PortIn`,
`PortOut`, reflection and registration conventions found in the upstream GR4
source. This repository does not pretend a GR4 build was verified when no
installable package is available; see `gr-nr-sib1/README.md`.

## Output

Events are externally tagged JSON objects. This is the verbatim output of
`cargo run -p nr-sib1 --example decode` on the public n3 fixture, compacted to
one event per line:

```json
{"Sync":{"pci":1,"nid1":0,"nid2":1,"sample":8399,"cfo_hz":-2834.286376953125,"ssb_hz":1876951165.713623,"quality_db":17.024930953979492,"ssb_index":0,"half_frame":false}}
{"Mib":{"pci":1,"system_frame":896,"half_frame":false,"subcarrier_spacing_common":"Khz15","ssb_subcarrier_offset":8,"dmrs_type_a_position":2,"pdcch_config_sib1":0,"cell_barred":false,"intra_frequency_reselection":false,"ssb_index":0}}
{"Sib1":{"pci":1,"bands":[3],"plmn":[{"mcc":"001","mnc":"01","country":null,"operator":"TEST"}],"tracking_area_code":7,"cell_identity":6576,"cell_barred":false,"downlink_bandwidth_prb":25,"tdd_pattern":null,"initial_uplink_hz":1779850000.0,"initial_uplink_bwp":{"location_and_bandwidth":6600,"subcarrier_spacing":"Khz15"},"prach":{"configuration_index":1,"message_one_fdm":1,"frequency_start":4,"root_sequence_index":1,"zero_correlation_zone":0}}}
```

A cell that synchronises but never reaches SIB1 emits a `Failure` instead,
preserving the stage it reached, the sample, the PCI where one was recovered and
the structured reasons — a cell that was never in the window keeps saying so
rather than reading as a weak signal.

## Tests and data

```sh
cargo fmt --all --check
cargo check --workspace --all-targets
cargo test -p nr-sib1 -p nr-sib1-ffi
cargo clippy --workspace --all-targets -- -D warnings
cargo +nightly-2026-08-15 test -p nr-sib1-futuresdr --features futuresdr
```

Fixture and impairment tests use only the public n3 srsRAN capture. The n3
fixture is CC BY 4.0, polar vectors are MIT, and project-generated LDPC vectors
are MIT. Attribution lives in `crates/nr-sib1/tests/fixtures/README.md`, the
fixture-local READMEs and `NOTICE`. No Bell, Rogers or other live operator IQ is
present.

The library is MIT licensed. 3GPP ASN.1 and specification tables retain their
original copyrights; see `NOTICE`.
