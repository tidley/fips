# FIPS Drop Protocol v0

Status: proof-of-concept wire contract.

FIPS Drop v0 is a small file-transfer protocol carried inside encrypted FIPS
service packets. It is not a public storage standard. It is the private
transport used by the Android-to-Pi PoC, with room for Blossom/Nostr metadata
above it.

## Transport Binding

- Carrier: FIPS FSP service data packet.
- Receiver service port: `4242`.
- Android reply port: `49152`.
- FIPS session: already established before file transfer begins.
- Security: inherited from the FIPS peer/session layer.
- Delivery model: datagram-like, encrypted, lossy, unordered.

## Binary Frame Header

Every v0 binary frame starts with:

```text
0..4  magic: ASCII "FDB1"
4     type: u8
```

All integer fields are little-endian. Text fields are UTF-8.

Length prefixes:

- `text8`: `u8 length` followed by UTF-8 bytes.
- `text16`: `u16 length` followed by UTF-8 bytes.
- `bytes16`: `u16 length` followed by raw bytes.
- Empty optional text means `None`.

## Frame Types

### `1` BlobStart

```text
magic      "FDB1"
type       1
id         text8
name       text16
mime       text16, empty if unknown
sha256     text8, lowercase hex
size       u64
chunk_size u16
chunk_count u32
```

Receiver behavior:

- Validate `name` is a single safe relative filename.
- Validate `chunk_size > 0`.
- Validate `chunk_count > 0`.
- Validate `ceil(size / chunk_size) == chunk_count`.
- Create or confirm compatible pending upload state for `id`.
- Reply with `BlobAck`.

### `2` BlobChunk

```text
magic       "FDB1"
type        2
id          text8
chunk_index u32
data        bytes16
```

Receiver behavior:

- Reject unknown upload `id`.
- Reject `chunk_index >= chunk_count`.
- Store chunk idempotently. Re-sending a chunk with the same index replaces the
  pending value.
- No per-chunk ACK is required.

### `3` BlobAck

```text
magic              "FDB1"
type               3
id                 text8
received_chunks    u32
highest_contiguous u32, or u32::MAX if none
missing_count      u16
missing_chunks     missing_count * u32
```

Receiver behavior:

- Sent after `BlobStart` and after every `BlobDone` when the upload is not yet
  complete.
- `missing_chunks` is sparse and bounded. It reports the earliest missing chunk
  indexes first.

Sender behavior:

- Ignore missing indexes above the highest chunk the sender has intentionally
  sent.
- Repair missing chunks in small batches.
- Ask for another report after each repair batch.

### `4` BlobDone

```text
magic "FDB1"
type  4
id    text8
```

Receiver behavior:

- If all chunks are present, assemble the file, validate size and SHA-256, write
  it to storage, and reply with `Stored`.
- If chunks are missing, reply with `BlobAck`.

### `5` Stored

```text
magic  "FDB1"
type   5
id     text8
sha256 text8
size   u64
path   text16
```

This is the final success acknowledgement.

### `6` Error

```text
magic  "FDB1"
type   6
id     text8, empty if not tied to one upload
reason text16
```

This is a terminal protocol/storage error for the referenced upload.

## Android Sender Profile

The current Android PoC profile starts conservatively:

- file data per chunk: `768` bytes.
- initial window: `32` chunks.
- adaptive window range: `8..64` chunks.
- initial repair batch: `8` chunks.
- adaptive repair batch range: `4..16` chunks.
- initial chunk spacing: `6 ms`.
- adaptive chunk spacing range: `3..20 ms`.
- window pause: `50 ms`.
- max file size: `10 MiB`.

Adaptation rule:

- Any sparse report with missing chunks in the sent prefix is treated as loss:
  halve the next window, reduce repair batch size, and increase spacing.
- Three consecutive clean windows allow cautious growth: increase window and
  repair batch size, and decrease spacing.
- A report timeout forces minimum window/batch and maximum spacing.

## Integrity

The receiver validates:

- declared total size,
- full-file SHA-256,
- filename safety,
- chunk count/chunk indexes.

The final `Stored` acknowledgement returns the receiver-computed SHA-256 and
size. The sender should treat missing final `Stored` as failure even if all
chunks were previously transmitted.

## Compatibility

The implementation still accepts the earlier CoAP/JSON Block1 codec used during
the first prototype. Android sends the `FDB1` binary form by default.

Internal Rust symbols still contain `dropbox` names in places for source-level
compatibility during the PoC transition. Product docs, logs, binaries, and
deployment material should use "FIPS Drop".

## Nostr/Blossom Alignment

FIPS Drop v0 is only the private transfer pipe. The next open-content layer
should publish or return a receipt containing:

- content SHA-256,
- size,
- MIME type,
- original filename,
- receiver npub,
- optional Blossom server URL/object URL,
- optional Nostr event id for metadata.

That receipt can be made Blossom-compatible without changing the private FIPS
transport frames.
