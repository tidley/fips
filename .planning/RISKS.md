## 2026-04-28 11:32 UTC
- `fips.key` contained a real-looking `nsec1...` and was tracked in this branch history before cleanup at tip. We removed key files from current tree/PR diff and added ignores, but **history still contains exposed key material**.
- Blocker for secure release posture: rotate/regenerate any potentially used key and (if required by policy) do authorized history rewrite in a separate approved operation.

## Android FIPS mobile via Nostr UDP holepunch blockers

- Android build compatibility is now proven through `crates/fips-mobile` and the
  Pushstr bridge, but native-library packaging must stay aligned with generated
  Flutter Rust Bridge bindings.
- The embedded API now wraps Nostr discovery -> traversal adoption -> session
  readiness; remaining risk is lifecycle cleanup during mobile network changes
  and app backgrounding.
- App-service send/receive exists for FSP service ports; remaining risk is
  backpressure, progress reporting, and resumability for larger transfers.
- Mobile runtime constraints: app lifecycle, Doze/background limits, network switching, and UDP socket lifetime may interrupt traversal/session state.
- NAT reality: Nostr/STUN UDP punching is proven but not universal; carrier-grade/symmetric NAT or restricted mobile networks may require peer-assist/helper fallback.
- Pushstr bridge/package gap is reduced: bindings and Android NDK builds exist,
  but dirty local app changes need clean commit boundaries and repeatable
  release packaging.
- Pi node readiness is proven manually; remaining risk is packaging,
  authorization/quota policy, and stable operator defaults.
