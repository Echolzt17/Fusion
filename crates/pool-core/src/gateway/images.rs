use super::*;

pub(super) async fn generate(
    State(ctx): State<Context>,
    h: HeaderMap,
    tracking: RequestTracking,
    Json(v): Json<Value>,
) -> Response {
    forward(ctx, h, v, false, tracking).await
}
pub(super) async fn edit(
    State(ctx): State<Context>,
    h: HeaderMap,
    tracking: RequestTracking,
    Json(v): Json<Value>,
) -> Response {
    forward(ctx, h, v, true, tracking).await
}
async fn forward(ctx: Context, h: HeaderMap, v: Value, edit: bool, tracking: RequestTracking) -> Response {
    let mut audit = Audit::tracked(ctx, if edit { "image_edit" } else { "image_generate" }, tracking);
    if !v.is_object() || v["stream"].as_bool() == Some(true) {
        return audit.error(
            StatusCode::BAD_REQUEST,
            "invalid_image_request",
            "当前原生图片接口需要非流式 JSON 对象",
        );
    }
    if edit
        && !v["images"]
            .as_array()
            .is_some_and(|images| (1..=5).contains(&images.len()))
    {
        return audit.error(
            StatusCode::BAD_REQUEST,
            "invalid_image_request",
            "图片编辑需要 1—5 张参考图",
        );
    }
    let (lease, token) = match prepare(&mut audit, &h, &v).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let path = if edit {
        "/images/edits"
    } else {
        "/images/generations"
    };
    // No Responses-only normalization, no arbitrary URL fetch, no credential forwarding.
    let req = match request(&mut audit, &lease, &token, &h, reqwest::Method::POST, path).await {
        Ok(r) => r.json(&v),
        Err(e) => return audit.network_error(e),
    };
    drop(v);
    audit.phase("catalog_headers");
    let response = match audit.send(req, IMAGE_TIMEOUT).await {
        Ok(r) => r,
        Err(e) => return audit.network_error(e),
    };
    audit.received(&response);
    if !response.status().is_success() {
        return rejected(&mut audit, &lease, response).await;
    }
    // Decode the bounded payload, as the native client does. MIME metadata alone
    // cannot invalidate an already generated result (and must never cause replay).
    let headers = response.headers().clone();
    let status = response.status();
    let body = match audit.read_response(response, MAX_EVENT_BYTES, IMAGE_TIMEOUT).await {
        Ok(b) => b,
        Err(e) => return audit.read_error(e),
    };
    let value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return audit.error(
                StatusCode::BAD_GATEWAY,
                "invalid_image_response",
                "上游图片响应格式错误",
            )
        }
    };
    audit.diagnostic.detected_response_type = Some(ResponseType::Json);
    audit.response_model = crate::usage::model_name(&value["model"]);
    if !value["data"].as_array().is_some_and(|a| {
        !a.is_empty()
            && a.iter()
                .all(|i| i["b64_json"].as_str().is_some_and(|s| !s.is_empty()))
    }) {
        return audit.error(
            StatusCode::BAD_GATEWAY,
            "invalid_image_response",
            "上游未返回图片数据",
        );
    }
    if let Err(e) = bind_output(&audit.ctx, &value, &lease.account.id) {
        return audit.error(StatusCode::CONFLICT, "continuity_unavailable", &e);
    }
    audit.usage = crate::usage::TokenUsage::from_response(&value);
    audit.success();
    // Only success evidence is retained, never image bytes or prompt text.
    crate::compatibility::record_image_success(&audit.ctx.accounts.store);
    let mut out = (
        status,
        [(http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response();
    copy_image_headers(&headers, out.headers_mut());
    out.headers_mut()
        .insert("x-request-id", audit.id.parse().unwrap());
    out
}
