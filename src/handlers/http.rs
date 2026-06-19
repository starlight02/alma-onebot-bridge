pub async fn health_handler() -> Result<impl warp::Reply, warp::Rejection> {
    Ok(warp::reply::json(&serde_json::json!({
        "status": "ok",
        "service": "alma-onebot-bridge"
    })))
}
