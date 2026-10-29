// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn egress_metadata_reads_are_bounded_and_preserve_not_found() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("record.json");
    assert_eq!(
        read_egress_record(&path).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    std::fs::write(&path, vec![b'x'; MAX_EGRESS_RECORD_BYTES + 1]).expect("oversized record");
    assert_eq!(
        read_egress_record(&path).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
}
