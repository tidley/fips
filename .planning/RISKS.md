## 2026-04-28 11:32 UTC
- `fips.key` contained a real-looking `nsec1...` and was tracked in this branch history before cleanup at tip. We removed key files from current tree/PR diff and added ignores, but **history still contains exposed key material**.
- Blocker for secure release posture: rotate/regenerate any potentially used key and (if required by policy) do authorized history rewrite in a separate approved operation.
