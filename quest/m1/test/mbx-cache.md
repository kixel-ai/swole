# [L] Cross-host content-addressed test-result caching via mbx-cache

## Goal

Hermetic passing test batches run under `swole test` are cached locally in `~/.cache/swole/test/` and shared across hosts over Tailscale via `mbx-cache`'s v1 HTTP API (`MBX_REMOTE_URL`, `MBX_REMOTE_TOKEN_FILE`, `MBX_REMOTE_NAMESPACE`, respecting `MBX_HTTP_RETRIES`), keyed by the test binary's blake3 digest, target triple, effective host CPU capabilities, `--timeout` policy, `test-preload.so` hash, batch test list, allowlisted env, and workspace fixture files and directory memberships auto-traced by `test-preload.so`, delivering 0-CPU cache hits for unchanged test binaries while never reading or writing cache entries for non-hermetic runs.

## Plan

Settled in [/quest/m1/test/README.md](/quest/m1/test/README.md): cross-host caching is a v1 requirement, speaking `mbx-cache` v1 `task` action results (`/v1/action-results`, `/v1/blobs`) and `/v1/action-manifests` directly, auto-tracing opened workspace files and enumerated directories into action manifests, never writing to the cache when `--no-sandbox`, `--real-time`, foreign runners, or outside-workspace reads are active, and exposing `--no-cache` to bypass lookup and upload.

Recommendations for the implementer:

- First inspect `mbx-cache`'s live `GET /v1/capabilities` and `task` schema v1 validator (against `http://127.0.0.1:8339` or `jdx/mr-boxington-cache`'s spec) before writing client code. If `mbx-cache`'s strict `task` validator rejects test action-result payloads for a reason that cannot be cleanly satisfied without hacks, stop and surface that finding to the maintainer rather than working around it.
- Extend `test-preload.so` to record every workspace path opened, stat'd, or enumerated during a hermetic batch (`open`, `open64`, `openat`, `openat64`, `opendir`, `fdopendir`, `readdir`, `readdir64`, `getdents64`, `stat`, `lstat`, `newfstatat`, `statx`, `readlink`, `readlinkat`) to an inherited log fd owned by the runner.
  - Canonicalize paths against the workspace root to produce portable repo-relative input paths (matching `mbx`'s `share_workspace_root` behavior so different worktree paths hit the same cache entries).
  - For every workspace directory enumerated at runtime, record a deterministic digest of its sorted entry names (and entry types) in the action manifest and re-verify directory membership on lookup so adding or removing a fixture file (such as a new `.apple.s` in `rind`'s assembly tests) invalidates the cached pass; bypass caching whenever directory reads cannot be tracked.
  - If a batch opens or enumerates a path outside the workspace, `TMPDIR`, and immutable system/toolchain read-only paths, mark that batch uncacheable and skip uploading its result.
- Include effective host CPU capabilities (the host ISA/feature flags exposed to user space, such as AVX/AVX2/AVX-512/XGETBV on x86_64 and NEON/SVE/crypto features on aarch64) and the `--timeout` policy in cache compatibility so a pass recorded on a less-capable CPU (where a feature-gated check like `rind`'s `upper_vector_state_left_clean` skips) or under a looser timeout cannot suppress a check on a more-capable host or stricter timeout; bypass reuse whenever capability or timeout equivalence cannot be established.
- Construct a two-stage lookup matching `mbx`'s manifest model:
  1. Primary manifest key: `blake3("swole-test-manifest-v1" || binary_hash || target || cpu_caps || timeout_policy || preload_hash || batch_tests || allowlisted_env)`.
  2. Fetch the action manifest (local disk first, then `mbx-cache` `/v1/action-manifests`). For each recorded input set in the manifest, hash the current workspace files and verify recorded directory memberships locally to compute the concrete action-result key; on match, fetch and replay the passing result (captured output + timings summary) with zero test execution.
  3. On miss, run the batch hermetically; if all tests in the batch pass and inputs were clean, upload the action result and update the manifest locally and to `mbx-cache` (honoring `MBX_REMOTE_TRUST_WRITES` and failing soft on unreachable remote with `MBX_HTTP_RETRIES=0`, never hanging on an offline Tailscale peer).
- Never cache failed or timed-out batches, and skip both reads and writes when `--no-cache`, `--no-sandbox`, `--real-time`, `--time-warp`, or a foreign target runner is active.
- Prove end-to-end with an automated integration test against an in-process mock `mbx-cache` v1 HTTP server (and smoke-tested against the host's local `mbx-cache` forwarder when reachable): cold run uploads manifest + action result, second run from a different worktree path gets a 0-exec cache hit, editing a traced fixture file or adding a new file to an enumerated fixture directory invalidates the hit and re-runs, changing CPU-capability fingerprint or tightening `--timeout` misses the cache, and `--no-sandbox` / `--real-time` / failing batches never write to the cache.

## Required

- [Core swole test runner](/quest/m1/test/runner.md) - runs the sharded test batches whose pass results are cached
- [Hermetic sandbox and strict test preload](/quest/m1/test/sandbox-and-time.md) - enforces hermeticity and provides `test-preload.so` to trace opened workspace files
