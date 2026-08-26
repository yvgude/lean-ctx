# Full-history baseline review

- Audit target: `bff280bfc8eba6959854fafcbf104b5745d794c3`
- Audit source: clean post-rewrite clone, all public branches and tags
- Scanner contract: `commit-path/v2`, `diff-pickaxe/v2`, `public-tree/v2`
- Coverage: 16,291 commits, 99,593 objects, 558 metadata-only findings
- Current tree: 11 accepted finding IDs

The previous 473 findings remain present with identical IDs. The 85 additional
object-specific IDs introduce zero new `(scanner, rule, path)` combinations.
They therefore represent already-audited path classes across later or rewritten
objects, not a newly accepted finding class.

No rule, forbidden-path entry, source-binding requirement, or allowlist was
weakened. The pending audit status remains intentional because remediation of
other historical classes is outside this baseline rotation.
