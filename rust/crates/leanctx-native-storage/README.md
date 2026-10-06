# LeanCTX native storage

Shared Apache-2.0 filesystem primitives used by the public host and private
components. This crate contains no entitlements, billing, product selection or
commercial rules.

`windows_private::Directory` retains verified local directory handles and
provides current-user private creation, bounded reads and atomic publication.
Keep the directory authority alive while using file handles returned from it.
Existing broad ACLs are refused rather than repaired. Remote volumes, reparse
paths and unverifiable objects fail closed.

`windows_file` is the lower-level handle-relative open API retained for existing
host artifact stores. It does not itself provide the complete ownership, ACL,
ancestor and link checks of `windows_private`.

Native Windows execution remains a separate qualification gate. Cross-building
this crate is not proof of a complete installed product workflow. Other targets
do not receive an emulated permission fallback.
