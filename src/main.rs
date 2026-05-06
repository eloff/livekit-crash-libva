//! Reproduction of a double-free crash in the LiveKit Rust SDK.
//!
//! When a video track is published via `publish_track` and the room is later
//! closed, internal libwebrtc resources are not properly released. Publishing
//! a video track in a subsequent room session within the same process then
//! crashes with `double free or corruption (out)` (SIGABRT).
//!
//! ## Prerequisites
//!
//! A local LiveKit dev server:
//!
//! ```sh
//! docker run --rm -p 7880:7880 -p 7881:7881 -p 7882:7882/udp \
//!     livekit/livekit-server:v1.11 \
//!     --dev --bind 0.0.0.0 --node-ip 127.0.0.1
//! ```
//!
//! ## Running
//!
//! ```sh
//! RUST_LOG=info cargo run
//! ```
//!
//! The program crashes with `double free or corruption (out)` / SIGABRT on the
//! second iteration, every time.

use std::process::ExitCode;
use std::time::Duration;

use futures_util::StreamExt;
use libwebrtc::prelude::{I420Buffer, RtcVideoSource, VideoFrame, VideoResolution, VideoRotation};
use libwebrtc::video_source::native::NativeVideoSource;
use livekit::id::ParticipantIdentity;
use livekit::options::{TrackPublishOptions, VideoCodec};
use livekit::prelude::*;
use livekit::{Room, RoomEvent, RoomOptions, StreamByteOptions, StreamWriter as _};
use livekit_api::access_token::{AccessToken, VideoGrants};
use tracing::info;

const LIVEKIT_URL: &str = "http://localhost:7880";
const API_KEY: &str = "devkey";
const API_SECRET: &str = "secret";

const NUM_ITERATIONS: u32 = 5;

fn generate_token(room_name: &str, identity: &str) -> String {
    let grants = VideoGrants {
        room_join: true,
        room: room_name.to_string(),
        ..Default::default()
    };
    AccessToken::with_api_key(API_KEY, API_SECRET)
        .with_identity(identity)
        .with_grants(grants)
        .to_jwt()
        .expect("failed to generate token")
}

/// Run a single session: connect, publish video + data tracks, exchange data, close.
async fn run_session(iteration: u32) {
    let room_name = format!("room-close-repro-{iteration}");

    // Connect A (gateway/device).
    let token_a = generate_token(&room_name, "device");
    let (room_a, mut events_a) = Room::connect(LIVEKIT_URL, &token_a, RoomOptions::default())
        .await
        .expect("A failed to connect");

    // Connect B (viewer).
    let token_b = generate_token(&room_name, "viewer");
    let (room_b, mut events_b) = Room::connect(LIVEKIT_URL, &token_b, RoomOptions::default())
        .await
        .expect("B failed to connect");

    wait_for_participant(&room_a, &mut events_a, "viewer").await;

    // A opens byte stream to B.
    let writer_a = room_a
        .local_participant()
        .stream_bytes(StreamByteOptions {
            topic: "control".to_string(),
            destination_identities: vec![ParticipantIdentity("viewer".to_string())],
            ..StreamByteOptions::default()
        })
        .await
        .expect("A stream_bytes failed");

    let mut reader_b = wait_for_byte_stream(&mut events_b).await;

    // B opens byte stream to A.
    let writer_b = room_b
        .local_participant()
        .stream_bytes(StreamByteOptions {
            topic: "control".to_string(),
            destination_identities: vec![ParticipantIdentity("device".to_string())],
            ..StreamByteOptions::default()
        })
        .await
        .expect("B stream_bytes failed");

    let mut reader_a = wait_for_byte_stream(&mut events_a).await;

    // A publishes a data track.
    let dt1 = room_a
        .local_participant()
        .publish_data_track("data-ch-1".to_string())
        .await
        .expect("publish dt1");

    let mut stream_1 = wait_for_data_track(&mut events_b, "data-ch-1").await;

    // A publishes a video track.
    let video_source = NativeVideoSource::new(
        VideoResolution { width: 320, height: 240 },
        false,
    );
    let video_track = LocalVideoTrack::create_video_track(
        "video-ch-1",
        RtcVideoSource::Native(video_source.clone()),
    );
    room_a
        .local_participant()
        .publish_track(
            LocalTrack::Video(video_track),
            TrackPublishOptions {
                video_codec: VideoCodec::H264,
                ..Default::default()
            },
        )
        .await
        .expect("publish video");

    // Push video frames.
    for _ in 0..3 {
        let mut buffer = I420Buffer::new(320, 240);
        let (y, u, v) = buffer.data_mut();
        y.fill(128);
        u.fill(128);
        v.fill(128);
        video_source.capture_frame(&VideoFrame::new(VideoRotation::VideoRotation0, buffer));
        tokio::time::sleep(Duration::from_millis(33)).await;
    }

    wait_for_track_subscribed(&mut events_b).await;

    // Exchange data on byte streams and data track.
    writer_a.write(b"hello").await.expect("write A failed");
    writer_b.write(b"world").await.expect("write B failed");
    if let Some(Ok(_)) = reader_b.next().await {}
    if let Some(Ok(_)) = reader_a.next().await {}

    dt1.try_push(DataTrackFrame::new(b"data".to_vec())).ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), stream_1.next()).await;

    // B disconnects.
    drop(reader_b);
    drop(stream_1);
    drop(writer_b);
    room_b.close().await.ok();

    // A closes with video + data tracks still published.
    room_a.close().await.expect("A room close failed");
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    info!("running {NUM_ITERATIONS} iterations within a single process");
    info!("expect a double-free crash (SIGABRT) on iteration 1");

    for i in 0..NUM_ITERATIONS {
        info!("--- iteration {i}/{NUM_ITERATIONS} ---");
        run_session(i).await;
    }

    info!("all {NUM_ITERATIONS} iterations completed without crash");
    ExitCode::SUCCESS
}

async fn wait_for_participant(
    room: &Room,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
    identity: &str,
) {
    if room
        .remote_participants()
        .values()
        .any(|p| p.identity().0 == identity)
    {
        return;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("timeout waiting for participant")
            .expect("events channel closed");
        if let RoomEvent::ParticipantConnected(p) = &event {
            if p.identity().0 == identity {
                return;
            }
        }
    }
}

async fn wait_for_byte_stream(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
) -> livekit::ByteStreamReader {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("timeout waiting for byte stream")
            .expect("events channel closed");
        if let RoomEvent::ByteStreamOpened { reader, .. } = event {
            return reader.take().expect("reader already taken");
        }
    }
}

async fn wait_for_data_track(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
    expected_name: &str,
) -> DataTrackStream {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .unwrap_or_else(|_| panic!("timeout waiting for data track '{expected_name}'"))
            .expect("events channel closed");
        if let RoomEvent::DataTrackPublished(track) = event {
            if track.info().name() == expected_name {
                return track
                    .subscribe()
                    .await
                    .expect("failed to subscribe to data track");
            }
        }
    }
}

async fn wait_for_track_subscribed(events: &mut tokio::sync::mpsc::UnboundedReceiver<RoomEvent>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("timeout waiting for track subscribed")
            .expect("events channel closed");
        if let RoomEvent::TrackSubscribed { .. } = event {
            return;
        }
    }
}
