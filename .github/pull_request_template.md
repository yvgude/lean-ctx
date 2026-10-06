## Summary

What does this PR change and why?

## Test plan

- [ ] `cd rust && cargo test -- --test-threads=1` (the suite shares process-global state; CI serializes it too)
- [ ] `cd rust && cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cd rust && cargo fmt --check`
- [ ] If cookbook/packages changed: relevant `npm test` / build steps

## Notes for reviewers

- Risk areas / edge cases:
- Backwards compatibility:
- Docs updated (links/files):

## Contributor License Agreement

First-time contributors: a bot will ask you to sign our one-time
[CLA](https://github.com/yvgude/lean-ctx/blob/main/CLA.md).
The existing CLA-v1 document and signature store remain in use. Sign by replying:
`I have read the CLA Document and I hereby sign the CLA`
