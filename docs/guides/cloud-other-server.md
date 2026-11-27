# Connecting to a Cloud other than the hosted one

lean-ctx ships the root public key of the hosted Cloud only. It verifies the
key set of every Cloud it connects to against a root key that it never takes
from the server it is checking. Any other server needs its root key in your
configuration: staging, a test server, or your own.

1. Ask the server's operator for its root public key. It is a base64 Ed25519
   public key, not a secret.
2. Add both values to the `[cloud]` section of `config.toml`.
   `lean-ctx config path` prints where that file is.

   ```toml
   [cloud]
   url = "https://cloud.example.internal"
   root_key = "<base64 root public key from the operator>"
   ```

3. Run `lean-ctx cloud connect`, or `lean-ctx cloud connect --url <server>`
   in place of the `url` line.

Under managed configuration, a managed `cloud.root_key` wins over
`[cloud] root_key` (see `docs/contracts/managed-config-v1.md`).

There is no command-line flag or environment variable for the root key, so a
copied command or a changed environment cannot replace the server you trust.

If `root_key` is set but `url` is missing, `cloud connect` refuses: the key
does not belong to the hosted Cloud. Set `url` as well.
