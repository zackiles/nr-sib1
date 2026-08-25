# n3 SSB and SIB1 fixture

Source: Daniel Estévez, [5G NR downlink](https://github.com/daniestevez/jupyter_notebooks/tree/master/5G),
licensed [CC BY 4.0](https://github.com/daniestevez/jupyter_notebooks/blob/master/LICENSE-CC-BY).
The capture was made by catkira from an srsRAN Project gNB and annotated by Estévez.

The files retain their original contents:

- `capture.sigmf-data` is `ci32_le` IQ at 7.68 MSps and 1.876954 GHz.
- `capture.sigmf-meta` contains sample-level PSS, SSS, PBCH, PDCCH and PDSCH annotations.
- `nr-rrc-17.3.0.asn1` is the matching RRC grammar used to decode the broadcast payload.
- `sib1-transport.bin` is the annotated SIB1 re-encoded with that grammar using UPER and padded
  from 75 bytes to its 80-byte transport-block size.

Ground truth used by this crate:

- band n3, 5 MHz, 15 kHz SCS
- PCI 1 (`NID1=0`, `NID2=1`)
- SSB index 0, `kSSB=8`
- SFN 886 in the first annotated frame
- `dmrs-TypeA-Position=pos2`
- `controlResourceSetZero=0`, `searchSpaceZero=0`
- cell not barred, intra-frequency reselection not allowed
- PLMN 001/01 and 25-RB carrier in SIB1

The SigMF metadata records SHA-512
`0bcd162b7fbcdd14b97088518cc682a26777e71a943cd7363dc917ab33b2900a91dc16ca04b0a8247468136aee28db475f1d03333bf36f47296b2264d565d869`
for the IQ file.
