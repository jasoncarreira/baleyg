# Extraction research evidence

Historical extraction results, timings, observations, source manifest, and browser
screenshots are retained here. See [summary](../EXTRACTION-RESULTS.md) and
[offline tools](../../../tools/extraction/README.md).

Source snapshots, SCIP indexes, expected data, graphs and hashes moved to
[`tests/fixtures/extraction`](../../../tests/fixtures/extraction/).
Artifacts were originally moved byte-for-byte. The public `source-manifest.json` and
`run-results.json` now replace local absolute paths with labelled placeholders and carry
publication notes; their exact originals are retained privately. Measurements and source
hashes are unchanged. Binary fixtures and images remain unchanged and can still contain original
local paths in metadata or pixels; the text redaction is not a binary/image anonymization pass.
Historical commands and
timestamps are not current runnable instructions. Current test reports are separate ignored
tool outputs.
