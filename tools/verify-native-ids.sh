set -eu
node docs/semantic-evidence/id-test-vectors/check.mjs
node docs/semantic-evidence/id-test-vectors/check.mjs --self-test
cargo test --locked --test native_id_vectors every_normative_stable_id_vector -- --exact --nocapture
./tools/verify
