# Polar fixture

`k40-e64-input.bin` and `k40-e64-codeword.bin` were extracted from
`polarencoder_testvec_K_40_E_64_isDL_1.mat` in
[python_5gtoolbox](https://github.com/hahaliu2001/python_5gtoolbox), licensed MIT.
That project generated its committed vectors with MATLAB 5G Toolbox. Each byte is one bit.

This fixture independently checks TS 38.212 polar input interleaving, frozen-position selection and
mother-code encoding. Rate matching and decoding tests use the resulting codeword.
