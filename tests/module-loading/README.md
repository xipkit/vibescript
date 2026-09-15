# Module path and policy reference fixtures

The fixtures contain 8,852 results from Go 1.27.1's `path.Match` and 1,004 module-policy observations from Vibescript v0.70.0: 988 runtime checks and 16 invalid configurations. Byte strings are encoded as hexadecimal so malformed UTF-8 remains reproducible.

The generator in `reference/` pins the Vibescript dependency and verifies the Go toolchain version. It tests real module loads through both allow and deny rules. A retained runner supplies the module origin for explicit relative requests; the receiving engine applies the selected policy. Temporary modules stay in the repository's `.cache/tmp` directory.

From the repository root, generate fresh output without replacing the checked-in fixtures:

```sh
mkdir -p .cache/language-modules/policy-reproduced
VIBE_POLICY_FIXTURES="$PWD/.cache/language-modules/policy-reproduced" ./scripts/go -C tests/module-loading/reference test -v -count=1 -timeout=30s
```

Expected SHA-256 digests:

| Fixture | SHA-256 |
| --- | --- |
| `glob-reference.json` | `c3f5e5f3589dd0870ab1bf529bec7e097b94c2b7081723034e64613193315047` |
| `policy-reference.json` | `52cbd0d66fcec760e66ded595de6de8643ba1db470131de4f568472e005d8e1f` |

`./scripts/cargo test --lib loading::` compares the Rust helpers with these results and checks path normalization, literal filename bytes, bounded work, cancellation, allocation limits, and scratch-buffer reclamation. Native filesystem checks cover ordered roots, explicit relative origins, policy enforcement before filesystem access, exact filename spelling, symlinks, bounded source reads and nonblocking rejection of Unix FIFOs. Temporary filesystem fixtures also stay under `.cache/tmp`.

Configured roots retain open directory handles. A component walk opens directories and files without following symlinks, resolves links explicitly within the retained root, and charges traversal and temporary storage. Relative links and absolute links pointing inside the configured root are supported. Filename policy and relative callers retain the requested alias. Root replacement cannot redirect an existing resolver or retained caller; cache keys distinguish separately opened roots even at the same pathname. Source metadata and contents come from the same opened file. Tests cover root replacement, concurrent symlink replacement, special files, link loops, and cache separation.

The compiled-entry cache preserves held versions through source changes and clearing. It bounds entry count, publishes the first concurrent compilation, invalidates only the observed stale entry, and prevents pre-clear work from refilling a cleared generation. Retired callback payloads are released outside its lock.

The helpers are not yet connected to script `require`/`load` calls. Request-cache bounds, per-call version pinning, exports, and retained file environments still need runtime integration and coverage. Native filesystem tests currently run on macOS; Linux and Windows behavior remain pending native platform verification.
