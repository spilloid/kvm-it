# protocol/

Wire protocol v1: [SPEC.md](SPEC.md) is normative. `gen_vectors.py` generates `vectors.json` (Rust tests) and
`vectors.txt` (C tests) with its own CRC/framing code, so a bug shared by both implementations is caught.
Regenerate with `python3 protocol/gen_vectors.py` and commit the outputs. Draft rationale and the reliability
design live in [../docs/protocol.md](../docs/protocol.md).
