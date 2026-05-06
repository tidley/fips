# FIPS Drop Blossom/Nostr Alignment

FIPS Drop should use FIPS for private delivery and use open Nostr/Blossom
concepts for content identity, receipts, and optional publication.

## Current PoC

- Android sends file bytes privately to a target FIPS node.
- Receiver writes the file to local filesystem storage.
- Receiver returns `sha256`, `size`, and local `path` in the final stored ACK.
- Nostr is used only for bootstrap/signalling, not for content metadata.

## Next Content Receipt

The sender should receive a structured receipt after storage:

```json
{
  "protocol": "fips-drop-receipt-v0",
  "sha256": "<hex>",
  "size": 3138121,
  "mime": "video/mp4",
  "name": "VID-20260505-WA0003.mp4",
  "receiver_npub": "npub1...",
  "stored_at": "2026-05-05T21:34:07Z",
  "local_path": "VID-20260505-WA0003.mp4",
  "blossom_url": null,
  "nostr_event_id": null
}
```

The private FIPS transfer remains complete without `blossom_url` or
`nostr_event_id`.

## Blossom-Compatible Mode

When enabled, the receiver should optionally publish or expose the stored blob
through a Blossom-compatible object store:

1. Receive over FIPS Drop.
2. Validate SHA-256 and size.
3. Store by content hash.
4. Return a receipt that includes the Blossom URL/object reference.
5. Optionally publish a Nostr metadata event that references the content hash
   and Blossom URL.

This keeps FIPS Drop as the private transport and lets Blossom remain the open
content-addressed storage/read side.

## Nostr Metadata Event

The event shape is intentionally not frozen yet. Minimum candidate fields:

- content SHA-256,
- size,
- MIME type,
- filename,
- receiver npub,
- Blossom URL when present,
- optional sender npub,
- optional expiration/retention policy.

The event should not leak private file contents. Whether filenames are public
metadata must be an explicit product decision.

## Open Questions

- Should the receiver publish metadata automatically, only on sender request,
  or never by default?
- Should the sender sign the receipt request separately from the FIPS session?
- Which NIP/event kind is the best fit for private transfer receipts?
- Should local-only Pi storage use the same content-addressed layout as Blossom?
