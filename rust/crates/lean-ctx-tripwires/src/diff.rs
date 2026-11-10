// SPDX-License-Identifier: Apache-2.0
//! Line diff: which candidate lines are added and which base lines removed.
//! Lines compare after trimming trailing whitespace. Deterministic; a middle
//! too large for the LCS table counts as wholly changed (more hits, never fewer).

use std::collections::BTreeSet;

const MAX_CELLS: usize = 4_000_000;

#[derive(Debug, Default, Clone)]
pub(crate) struct LineDiff {
    /// 1-based candidate lines not in the base.
    pub added: BTreeSet<u32>,
    /// 1-based base lines not in the candidate.
    pub removed: BTreeSet<u32>,
}

impl LineDiff {
    pub(crate) fn is_added(&self, line: u32) -> bool {
        self.added.contains(&line)
    }

    pub(crate) fn added_in(&self, start: u32, end: u32) -> bool {
        self.added.range(start..=end).next().is_some()
    }

    pub(crate) fn removed_in(&self, start: u32, end: u32) -> bool {
        self.removed.range(start..=end).next().is_some()
    }
}

fn line_no(idx: usize) -> u32 {
    u32::try_from(idx + 1).unwrap_or(u32::MAX)
}

#[allow(clippy::many_single_char_names)]
pub(crate) fn diff_lines(base: &str, cand: &str) -> LineDiff {
    let a: Vec<&str> = base.lines().map(str::trim_end).collect();
    let b: Vec<&str> = cand.lines().map(str::trim_end).collect();
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf]
    {
        suf += 1;
    }
    let am = &a[pre..a.len() - suf];
    let bm = &b[pre..b.len() - suf];
    let mut out = LineDiff::default();
    if am.is_empty() || bm.is_empty() || am.len().saturating_mul(bm.len()) > MAX_CELLS {
        out.removed.extend((0..am.len()).map(|i| line_no(pre + i)));
        out.added.extend((0..bm.len()).map(|j| line_no(pre + j)));
        return out;
    }
    let (n, m) = (am.len(), bm.len());
    // lcs[i][j] = LCS length of am[i..] and bm[j..].
    let mut lcs = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[at(i, j)] = if am[i] == bm[j] {
                lcs[at(i + 1, j + 1)] + 1
            } else {
                lcs[at(i + 1, j)].max(lcs[at(i, j + 1)])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if am[i] == bm[j] {
            i += 1;
            j += 1;
        } else if lcs[at(i + 1, j)] >= lcs[at(i, j + 1)] {
            out.removed.insert(line_no(pre + i));
            i += 1;
        } else {
            out.added.insert(line_no(pre + j));
            j += 1;
        }
    }
    out.removed.extend((i..n).map(|k| line_no(pre + k)));
    out.added.extend((j..m).map(|k| line_no(pre + k)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn added_and_removed() {
        let d = diff_lines("a\nb\nc\n", "a\nx\nc\nd\n");
        assert_eq!(d.added.iter().copied().collect::<Vec<_>>(), vec![2, 4]);
        assert_eq!(d.removed.iter().copied().collect::<Vec<_>>(), vec![2]);
        let same = diff_lines("a\n", "a  \n");
        assert!(same.added.is_empty() && same.removed.is_empty());
    }
}
