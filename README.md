# LiveKit Rust SDK: double-free crash with video tracks

Minimal reproduction of a memory corruption bug in the LiveKit Rust SDK.

## Problem

When a video track is published to a LiveKit room via `publish_track`, and
then the room is closed, internal libwebrtc resources are not properly
released. Publishing a video track in a subsequent room session within the
same process then crashes with a **double-free** (`SIGABRT`).

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
| `134` | SIGABRT — double-free crash (expected) |
