// SPDX-License-Identifier: Apache-2.0

//! Windows prefix matching must respect path component boundaries.

use super::is_under_prefix_windows;
use std::path::Path;

#[test]
fn prefix_match_respects_component_boundaries() {
    let root = Path::new(r"C:\proj");
    assert!(is_under_prefix_windows(Path::new(r"C:\proj"), root));
    assert!(is_under_prefix_windows(
        Path::new(r"c:\PROJ\src\a.rs"),
        root
    ));
    assert!(is_under_prefix_windows(
        Path::new(r"\\?\C:\proj\a.rs"),
        Path::new(r"C:\proj\")
    ));
    assert!(!is_under_prefix_windows(Path::new(r"C:\proj-evil\x"), root));
    assert!(!is_under_prefix_windows(Path::new(r"C:\projx"), root));
}
