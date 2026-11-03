// SPDX-License-Identifier: Apache-2.0
//! The import tag of a fact pulled from a Hive vault: `imported_from` reads
//! `hive-repo:@ns/name@rN`. Facts with such a tag are other members'
//! knowledge; every other origin is local or a context package.

use super::KnowledgeFact;

/// `imported_from` of every fact pulled from a Hive vault starts with this.
/// It is stored in the project knowledge, so it keeps its pre-VA name: an
/// older engine (downgrade) still recognizes and replaces these facts.
pub const IMPORT_TAG_SCHEME: &str = "hive-repo:";
/// The vault spelling, read as well (written by development builds).
const VAULT_IMPORT_TAG_SCHEME: &str = "hive-vault:";

/// A Hive import tag without its scheme (`@ns/name@rN`), `None` for every
/// other origin.
pub fn import_tag_source(origin: &str) -> Option<&str> {
    origin
        .strip_prefix(IMPORT_TAG_SCHEME)
        .or_else(|| origin.strip_prefix(VAULT_IMPORT_TAG_SCHEME))
}

/// Whether a fact came from a Hive vault (a pulled revision).
#[must_use]
pub fn is_hive_fact(fact: &KnowledgeFact) -> bool {
    fact.imported_from
        .as_deref()
        .and_then(import_tag_source)
        .is_some()
}
