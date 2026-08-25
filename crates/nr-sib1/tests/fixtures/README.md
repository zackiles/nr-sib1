# Fixtures

Only redistributable test data belongs here. It lives inside the crate so that a
published `nr-sib1` package tests itself without fetching anything.

- `n3-sib1/` is Daniel Estévez's annotated n3 capture, licensed CC BY 4.0. The
  original recording was made by catkira from an srsRAN Project gNB.
- `polar/` vectors come from `python_5gtoolbox` and are MIT licensed.
- `ldpc/` vectors were generated for this project and are distributed under the
  repository's licence.

No live operator capture and no Canadian commercial-network IQ is included.
