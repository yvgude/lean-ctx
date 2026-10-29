// SPDX-License-Identifier: Apache-2.0
//! Text from this engine's own repositories never goes into a public swarm
//! (R3-3e-2). The secret and PII screening does not see ordinary code, so
//! before every write to a public swarm the text is compared verbatim with
//! the files of every project linked to Hive on this machine: tracked
//! ones, and untracked ones git does not ignore (fresh notes; `.env` and
//! the like stay ignored).
//!
//! A hit is three consecutive significant lines of one file, or 200
//! contiguous bytes of one file. Bytes: each file is cut into 100-byte
//! blocks at fixed offsets, and every 100-byte window of the text is looked
//! up; a copied run of 200 bytes always covers a whole block. Blocks and
//! lines that are mostly whitespace or punctuation do not count. The
//! refusal names no file and no line.
//!
//! The index is kept per process and rebuilt only when a checkout changes
//! (its HEAD, `git status`, or the size or mtime of a changed or untracked
//! file). Files over 1 MiB, binary files and symlinks are not compared. In
//! very large checkouts only the first 50,000 files or 256 MiB, in git's
//! path order, are compared; always the same ones.

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use super::HiveError;
use super::state::HiveStore;
use super::vault::VaultMap;

/// Consecutive significant lines that make a hit.
const RUN_LINES: usize = 3;
/// A significant line has at least this many characters (trimmed).
const MIN_LINE_CHARS: usize = 8;
/// Block length; a hit needs twice this many contiguous bytes at most.
const BLOCK_BYTES: usize = 100;
/// A block counts only with this many non-whitespace bytes.
const MIN_BLOCK_INK: usize = 40;
const MAX_FILES: usize = 50_000;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// Refuse `fields` (name, text) if any holds text of a linked repository.
pub fn check(store: &HiveStore, fields: &[(&str, &str)]) -> Result<(), HiveError> {
    if fields.iter().all(|(_, text)| text.trim().is_empty()) {
        return Ok(());
    }
    let roots: Vec<String> = VaultMap::load(store.data_dir())
        .vaults
        .into_values()
        .flat_map(|project| project.roots)
        .collect();
    let index = cached_index(&roots)?;
    for (name, text) in fields {
        if index.holds(text) {
            return Err(HiveError::Refused(format!(
                "the {name} was not sent: it contains text from your own repositories"
            )));
        }
    }
    Ok(())
}

fn unchecked() -> HiveError {
    HiveError::Refused(
        "not sent: the text could not be checked against your own repositories".into(),
    )
}

/// `git -C root args…` as bytes; a failing git refuses (fail closed).
fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, HiveError> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| out.stdout)
        .ok_or_else(unchecked)
}

/// What the index of `roots` depends on: per checkout its HEAD, its
/// status, and the size and mtime of every file the status names.
fn fingerprint(roots: &[String]) -> Result<u64, HiveError> {
    let mut hasher = DefaultHasher::new();
    for root in roots.iter().map(Path::new).filter(|r| r.is_dir()) {
        root.hash(&mut hasher);
        // An empty repository has no HEAD yet.
        git(root, &["rev-parse", "-q", "--verify", "HEAD"])
            .unwrap_or_default()
            .hash(&mut hasher);
        let status = git(root, &["status", "--porcelain=v1", "-z", "-uall"])?;
        for entry in status.split(|b| *b == 0).filter(|e| e.len() > 3) {
            entry.hash(&mut hasher);
            // `XY path`; the original path of a rename follows bare and
            // simply does not stat.
            if let Ok(meta) =
                std::fs::symlink_metadata(root.join(String::from_utf8_lossy(&entry[3..]).as_ref()))
            {
                meta.len().hash(&mut hasher);
                meta.modified().ok().hash(&mut hasher);
            }
        }
    }
    Ok(hasher.finish())
}

static CACHE: Mutex<Option<(u64, Arc<Index>)>> = Mutex::new(None);

fn cached_index(roots: &[String]) -> Result<Arc<Index>, HiveError> {
    let key = fingerprint(roots)?;
    if let Some((cached, index)) = CACHE.lock().ok().and_then(|c| c.clone())
        && cached == key
    {
        return Ok(index);
    }
    let index = Arc::new(Index::of_roots(roots.iter().map(Path::new))?);
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((key, index.clone()));
    }
    Ok(index)
}

fn hash(value: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn significant_lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|line| {
            line.chars().count() >= MIN_LINE_CHARS && line.chars().any(char::is_alphanumeric)
        })
        .collect()
}

fn inked(block: &[u8]) -> bool {
    block.iter().filter(|b| !b.is_ascii_whitespace()).count() >= MIN_BLOCK_INK
}

#[derive(Default)]
struct Index {
    runs: HashSet<u64>,
    blocks: HashSet<u64>,
}

impl Index {
    fn of_roots<'p>(roots: impl Iterator<Item = &'p Path>) -> Result<Self, HiveError> {
        let mut index = Self::default();
        let (mut files, mut total) = (0usize, 0u64);
        for root in roots {
            // A checkout that is gone holds nothing to leak.
            if !root.is_dir() {
                continue;
            }
            // Tracked and untracked-but-not-ignored files, in git's path
            // order, so the caps always keep the same files.
            let listed = git(root, &["ls-files", "-z", "-co", "--exclude-standard"])?;
            for name in listed.split(|b| *b == 0).filter(|n| !n.is_empty()) {
                if files >= MAX_FILES || total >= MAX_TOTAL_BYTES {
                    return Ok(index);
                }
                let path = root.join(String::from_utf8_lossy(name).as_ref());
                let Ok(meta) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let Ok(text) = std::str::from_utf8(&bytes) else {
                    continue;
                };
                files += 1;
                total += meta.len();
                index.add(text);
            }
        }
        Ok(index)
    }

    fn add(&mut self, text: &str) {
        for run in significant_lines(text).windows(RUN_LINES) {
            self.runs.insert(hash(run));
        }
        // Hashed as a slice, like the windows `holds` compares.
        for block in text.as_bytes().as_chunks::<BLOCK_BYTES>().0 {
            if inked(block) {
                self.blocks.insert(hash(block.as_slice()));
            }
        }
    }

    fn holds(&self, text: &str) -> bool {
        if significant_lines(text)
            .windows(RUN_LINES)
            .any(|run| self.runs.contains(&hash(run)))
        {
            return true;
        }
        text.as_bytes()
            .windows(BLOCK_BYTES)
            .any(|window| inked(window) && self.blocks.contains(&hash(window)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "\
use std::collections::BTreeMap;

/// Settle every open invoice of the quarter.
pub fn reconcile_quarterly_settlements(ledger: &mut BTreeMap<u64, i64>) -> i64 {
    let mut balance = 0;
    for (invoice, amount) in ledger.iter_mut() {
        balance += *amount * i64::try_from(*invoice % 7).unwrap_or(1);
        *amount = 0;
    }
    balance
}
";

    fn index() -> Index {
        let mut index = Index::default();
        index.add(SOURCE);
        index
    }

    #[test]
    fn three_copied_lines_are_a_hit_even_reindented() {
        let pasted = "Here is how I would do it:\n\
            let mut balance = 0;\n\
            for (invoice, amount) in ledger.iter_mut() {\n\
            balance += *amount * i64::try_from(*invoice % 7).unwrap_or(1);\n";
        assert!(index().holds(pasted));
        // Two lines are not.
        let two = "let mut balance = 0;\nfor (invoice, amount) in ledger.iter_mut() {\n";
        assert!(!index().holds(two));
    }

    #[test]
    fn two_hundred_contiguous_bytes_are_a_hit_wherever_they_start() {
        for start in [0, 13, 57, 99, 140] {
            let run = &SOURCE[start..start + 200];
            // Flattened into one line, so no line run matches.
            let flat = format!("see: {}", run.replace('\n', " "));
            assert!(!index().holds(&flat), "flattening breaks the bytes too");
            assert!(index().holds(&format!("see: {run} end")), "start {start}");
        }
    }

    #[test]
    fn own_words_and_common_short_lines_are_no_hit() {
        let answer = "The quarter's balance is the sum of each amount weighted by the \
            invoice number modulo seven; amounts are reset to zero afterwards.\n\
            }\n\
            }\n\
            balance\n";
        assert!(!index().holds(answer));
        assert!(!index().holds(&" ".repeat(400)));
    }

    #[test]
    fn a_linked_checkout_is_read_from_its_files_except_ignored_ones() {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(repo.path())
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        git(&["init", "-q"]);
        std::fs::write(repo.path().join("ledger.rs"), SOURCE).unwrap();
        git(&["add", "ledger.rs"]);
        let ignored = "let ignored_sauce_formula = 42;\nlet ignored_sauce_formula = 43;\nlet ignored_sauce_formula = 44;\n";
        std::fs::write(repo.path().join(".gitignore"), "local.env\n").unwrap();
        std::fs::write(repo.path().join("local.env"), ignored).unwrap();
        let roots = vec![
            repo.path().to_string_lossy().into_owned(),
            repo.path().join("gone").to_string_lossy().into_owned(),
        ];
        let index = cached_index(&roots).unwrap();
        assert!(index.holds(&SOURCE[40..260]), "tracked");
        assert!(!index.holds(ignored), "ignored files stay out");

        // A fresh untracked note counts too, and the cached index sees it.
        let note = "let fresh_sauce_formula = 42;\nlet fresh_sauce_formula = 43;\nlet fresh_sauce_formula = 44;\n";
        assert!(!cached_index(&roots).unwrap().holds(note));
        std::fs::write(repo.path().join("notes.md"), note).unwrap();
        assert!(cached_index(&roots).unwrap().holds(note), "untracked");
        // An edit to a file already untracked rebuilds it as well.
        let edited = "let edited_sauce_formula = 1;\nlet edited_sauce_formula = 2;\nlet edited_sauce_formula = 3;\n";
        std::fs::write(repo.path().join("notes.md"), edited).unwrap();
        assert!(cached_index(&roots).unwrap().holds(edited), "edited");
    }

    /// `cargo test --lib -- --ignored --nocapture own_text::tests::index_time`
    /// prints how long this checkout takes to index, then to check again.
    #[test]
    #[ignore = "measurement"]
    fn index_time_of_this_checkout() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let roots = vec![root.to_string_lossy().into_owned()];
        let started = std::time::Instant::now();
        let index = Index::of_roots(roots.iter().map(Path::new)).unwrap();
        let built = started.elapsed();
        let started = std::time::Instant::now();
        cached_index(&roots).unwrap();
        let first = started.elapsed();
        let started = std::time::Instant::now();
        cached_index(&roots).unwrap();
        let again = started.elapsed();
        println!(
            "own_text: {} runs, {} blocks; build {built:?}, first check {first:?}, cached check {again:?}",
            index.runs.len(),
            index.blocks.len()
        );
    }
}
