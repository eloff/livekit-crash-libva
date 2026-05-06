# LiveKit Rust SDK: `Room::close()` memory corruption with video tracks

Minimal reproduction of a memory corruption bug in `livekit::Room::close()`.

## Problem

When a video track is published to a LiveKit room via `publish_track`, and
then the room is closed with `room.close()`, subsequent video track usage in
the same process crashes with a **double-free** (`SIGABRT`). The first
session closes without error, but the second session crashes when trying to
publish a new video track.

This manifests in our CI as:
- Sporadic test timeouts (undefined behavior from memory corruption appearing
  as deadlocks)
- `double free or corruption (out)` crashes under certain scheduling

The underlying issue appears to be that `Room::close()` doesn't properly
release video track resources (likely in the libwebrtc C++ layer). After
`close()` returns, some internal state remains corrupted, which causes a
double-free on the next allocation in the same process.

## Reproduction

The bug reproduces **every time** — iteration 0 completes normally, but
iteration 1 crashes:

```
--- iteration 0/5 ---
[...connects, publishes video, exchanges data, closes successfully...]
--- iteration 1/5 ---
[...connects, publishes video...]
double free or corruption (out)
Aborted (core dumped)
```

## Environment

- `livekit` crate version: 0.7.37
- `libwebrtc` crate version: 0.3.24
- LiveKit server: v1.11 (docker `livekit/livekit-server:v1.11`)
- Rust edition: 2024
- OS: Ubuntu 24.04, Linux 6.17

## Prerequisites

Run a local LiveKit dev server:

```sh
docker run --rm -p 7880:7880 -p 7881:7881 -p 7882:7882/udp \
    livekit/livekit-server:v1.11 \
    --dev --bind 0.0.0.0 --node-ip 127.0.0.1
```

## Running

```sh
RUST_LOG=info cargo run
```

## Exit status

| Code | Meaning |
|------|---------|
| `0` | All iterations passed (no bug) |
| `1` | `room.close()` timed out (hang variant of the bug) |
| `134` | SIGABRT — double-free crash (the primary reproduction) |

## Workaround

We work around the hang variant in production with a timeout:
```rust
match tokio::time::timeout(Duration::from_secs(5), room.close()).await {
    Ok(Ok(())) => {}
    Ok(Err(e)) => error!("failed to close room: {e}"),
    Err(_) => warn!("room close timed out; abandoning teardown"),
}
```

However, the double-free cannot be worked around — it corrupts the process.
The only mitigation is to ensure that video tracks are never published in a
process that will also create subsequent LiveKit room sessions.
