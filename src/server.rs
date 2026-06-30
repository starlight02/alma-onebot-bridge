use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;

use tracing::info;
use trillium::{Conn, Headers, Status};
use trillium_router::{Router, RouterConnExt};
use trillium_smol::SmolRuntime;
use trillium_websockets::{WebSocketConn, websocket};

use crate::handlers;
use crate::state::SharedState;

const MAX_HTTP_JSON_BODY_BYTES: usize = 64 * 1024;

pub async fn run_server_until<F>(
    port: u16,
    state: SharedState,
    shutdown_signal: F,
) -> Result<(), String>
where
    F: Future<Output = ()> + Send + 'static,
{
    let swansong = trillium_smol::Swansong::new();
    let shutdown_swansong = swansong.clone();
    SmolRuntime::default().spawn(async move {
        shutdown_signal.await;
        info!("Shutdown signal received; stopping Trillium HTTP/WebSocket server");
        shutdown_swansong.shut_down();
    });

    trillium_smol::config()
        .with_host("0.0.0.0")
        .with_port(port)
        .without_signals()
        .with_swansong(swansong)
        .run_async(router(state))
        .await;

    Ok(())
}

fn router(state: SharedState) -> Router {
    Router::new()
        .get("/health", |conn: Conn| async move {
            handlers::http::health_response().into_conn(conn)
        })
        .get("/qq/groups", {
            let state = state.clone();
            move |conn: Conn| {
                let state = state.clone();
                async move { list_groups(conn, state).await }
            }
        })
        .post("/qq/group/:group_id/send", {
            let state = state.clone();
            move |conn: Conn| {
                let state = state.clone();
                async move { send_group_message(conn, state).await }
            }
        })
        .post("/qq/private/:user_id/send", {
            let state = state.clone();
            move |conn: Conn| {
                let state = state.clone();
                async move { send_private_message(conn, state).await }
            }
        })
        .get("/", websocket_handler(state.clone()))
        .get("/ws", websocket_handler(state.clone()))
        .get("/onebot/v11/ws", websocket_handler(state))
}

fn websocket_handler(state: SharedState) -> impl trillium::Handler + 'static {
    websocket(move |ws: WebSocketConn| {
        let state = state.clone();
        async move {
            let auth_header = header_string(ws.headers(), "authorization");
            let query = query_map(ws.querystring());
            handlers::ws::handle_ws_connection(ws, state, auth_header, query).await;
        }
    })
}

async fn list_groups(conn: Conn, state: SharedState) -> Conn {
    let auth_header = header_string(conn.request_headers(), "authorization");
    let query = query_map(conn.querystring());
    let remote = remote_socket_addr(&conn);
    handlers::http::list_groups_handler(state, auth_header, query, remote)
        .await
        .into_conn(conn)
}

async fn send_group_message(mut conn: Conn, state: SharedState) -> Conn {
    let Some(group_id) = parse_route_id(&conn, "group_id") else {
        return json_error("invalid group_id", Status::BadRequest).into_conn(conn);
    };
    let auth_header = header_string(conn.request_headers(), "authorization");
    let query = query_map(conn.querystring());
    let remote = remote_socket_addr(&conn);
    let body = match read_send_body(&mut conn).await {
        Ok(body) => body,
        Err(response) => return response.into_conn(conn),
    };

    handlers::http::send_group_message_handler(group_id, body, state, auth_header, query, remote)
        .await
        .into_conn(conn)
}

async fn send_private_message(mut conn: Conn, state: SharedState) -> Conn {
    let Some(user_id) = parse_route_id(&conn, "user_id") else {
        return json_error("invalid user_id", Status::BadRequest).into_conn(conn);
    };
    let auth_header = header_string(conn.request_headers(), "authorization");
    let query = query_map(conn.querystring());
    let remote = remote_socket_addr(&conn);
    let body = match read_send_body(&mut conn).await {
        Ok(body) => body,
        Err(response) => return response.into_conn(conn),
    };

    handlers::http::send_private_message_handler(user_id, body, state, auth_header, query, remote)
        .await
        .into_conn(conn)
}

fn parse_route_id(conn: &Conn, name: &str) -> Option<i64> {
    conn.param(name)?.parse::<i64>().ok()
}

async fn read_send_body(
    conn: &mut Conn,
) -> Result<handlers::http::SendMessageRequest, handlers::http::JsonResponse> {
    if let Some(len) = conn
        .request_headers()
        .get_str("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        && len > MAX_HTTP_JSON_BODY_BYTES
    {
        return Err(json_error(
            "request body too large",
            Status::PayloadTooLarge,
        ));
    }

    let body = conn.request_body_string().await.map_err(|e| {
        json_error(
            &format!("failed to read request body: {}", e),
            Status::BadRequest,
        )
    })?;
    if body.len() > MAX_HTTP_JSON_BODY_BYTES {
        return Err(json_error(
            "request body too large",
            Status::PayloadTooLarge,
        ));
    }

    serde_json::from_str(&body)
        .map_err(|e| json_error(&format!("invalid JSON body: {}", e), Status::BadRequest))
}

fn remote_socket_addr(conn: &Conn) -> Option<SocketAddr> {
    conn.peer_ip().map(|ip| SocketAddr::new(ip, 0))
}

fn query_map(query: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
}

fn header_string(headers: &Headers, name: &str) -> Option<String> {
    headers
        .get_str(name)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn json_error(message: &str, status: Status) -> handlers::http::JsonResponse {
    handlers::http::JsonResponse::new(
        status,
        serde_json::json!({
            "status": "error",
            "error": message,
        }),
    )
}
