use std::time::Duration;

use axum::{
    Json,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};

use crate::AppState;

pub(crate) async fn log_request(
    State(state): State<AppState>,
) -> impl IntoResponse {
    if state.jawn_latency > 0 {
        tokio::time::sleep(Duration::from_millis(state.jawn_latency.into()))
            .await;
    }
    StatusCode::OK
}

pub(crate) async fn sign_s3_url(
    State(state): State<AppState>,
) -> impl IntoResponse {
    if state.jawn_latency > 0 {
        crate::routes::sleep(state.jawn_latency).await;
    }
    let s3_base_url = format!("http://{}:{}", state.address, state.port);
    let presigned_url = format!(
        "{s3_base_url}/request-response-storage/organizations/\
         c3bc2b69-c55c-4dfc-8a29-47db1245ee7c/requests/\
         a41cbcd7-5e9e-4104-b29b-2ef4473d71a7/raw_request_response_body"
    );
    let response = serde_json::json!({
        "data": {
            "url": presigned_url
        }
    });
    Json(response)
}

pub(crate) async fn websocket_handler(
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| websocket(socket))
}

// This function deals with a single websocket connection, i.e., a single
// connected client / user, for which we will spawn two independent tasks (for
// receiving / sending chat messages).
async fn websocket(stream: WebSocket) {
    // By splitting, we can send and receive at the same time.
    let (mut sender, mut receiver) = stream.split();

    // The old control-plane websocket payload types were removed from
    // ai-gateway. Keep this endpoint as a lightweight stub so the mock server
    // still starts and clients can connect during local development.
    let ready_message = serde_json::json!({
        "type": "ready",
        "source": "mock-server"
    });
    let message = Message::Text(ready_message.to_string().into());
    if sender.send(message).await.is_err() {
        return;
    }

    // Loop until a text message is found.
    while let Some(Ok(message)) = receiver.next().await {
        match message {
            Message::Text(utf8_bytes) => {
                tracing::info!("Received text message: {}", utf8_bytes);
            }
            Message::Binary(bytes) => {
                tracing::info!("Received binary message: {:?}", bytes);
            }
            Message::Ping(bytes) => {
                tracing::info!("Received ping message: {:?}", bytes);
                if sender.send(Message::Pong(bytes)).await.is_err() {
                    break;
                }
            }
            Message::Pong(bytes) => {
                tracing::info!("Received pong message: {:?}", bytes);
            }
            Message::Close(close_frame) => {
                tracing::info!("Received close message: {:?}", close_frame);
                break;
            }
        }
    }
}
