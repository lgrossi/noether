use axum::http::StatusCode;
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

pub fn mock_json(path: &str) -> (StatusCode, &'static str, String) {
    if path == "/v1/messages" {
        (
            StatusCode::OK,
            "application/json",
            json!({
                "id": format!("msg_{}", Uuid::new_v4()),
                "type": "message",
                "role": "assistant",
                "model": "noether-mock",
                "content": [{ "type": "text", "text": "Noether mock response" }],
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": { "input_tokens": 1, "output_tokens": 4 }
            })
            .to_string(),
        )
    } else {
        (
            StatusCode::OK,
            "application/json",
            json!({
                "id": format!("chatcmpl-{}", Uuid::new_v4()),
                "object": "chat.completion",
                "created": Utc::now().timestamp(),
                "model": "noether-mock",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "Noether mock response" },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 1, "completion_tokens": 4, "total_tokens": 5 }
            })
            .to_string(),
        )
    }
}

pub fn mock_stream(path: &str) -> (StatusCode, &'static str, String) {
    if path == "/v1/messages" {
        (
            StatusCode::OK,
            "text/event-stream",
            [
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_mock\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"noether-mock\",\"content\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Noether mock response\"}}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            ]
            .concat(),
        )
    } else {
        (
            StatusCode::OK,
            "text/event-stream",
            [
                "data: {\"id\":\"chatcmpl-mock\",\"object\":\"chat.completion.chunk\",\"model\":\"noether-mock\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Noether mock response\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"chatcmpl-mock\",\"object\":\"chat.completion.chunk\",\"model\":\"noether-mock\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            ]
            .concat(),
        )
    }
}
