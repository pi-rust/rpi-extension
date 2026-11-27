//! HTTP extraction and SSE encoding only; native observation semantics live in core.
use super::{
    events::authorized_events,
    response::{strict_json, typed_reply},
    AppState, HttpReply,
};
use crate::security::{self, RequestAuthorization};
use axum::{
    extract::{rejection::JsonRejection, Extension, Json, Path, Query, State},
    response::{sse::Event, IntoResponse, Sse},
};
use futures_util::{stream, StreamExt};
use rpi_extcore::{
    reply::IntoReply,
    service::delegations::{
        self, DelegationCancelQuery, DelegationEventQuery, DelegationStreamItem,
    },
};
use serde::Deserialize;

async fn roster(State(state): State<AppState>, Path(id): Path<String>) -> HttpReply {
    typed_reply(delegations::roster(&state.backend, &id).await)
}
async fn events_query(
    State(state): State<AppState>,
    Path((id, child)): Path<(String, String)>,
    query: Result<Json<DelegationEventQuery>, JsonRejection>,
) -> Result<HttpReply, HttpReply> {
    Ok(typed_reply(
        delegations::events(&state.backend, &id, &child, strict_json(query)?).await,
    ))
}
async fn cancel(
    State(state): State<AppState>,
    Extension(authorization): Extension<RequestAuthorization>,
    Path((id, child)): Path<(String, String)>,
    query: Result<Json<DelegationCancelQuery>, JsonRejection>,
) -> axum::response::Response {
    let Some(_commit_fence) = security::commit_fence(&state.security, &authorization).await else {
        return security::unauthorized_response();
    };
    let query = match strict_json(query) {
        Ok(query) => query,
        Err(error) => return error.into_response(),
    };
    typed_reply(delegations::cancel(&state.backend, &id, &child, query).await).into_response()
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StreamQuery {
    epoch: String,
    after_seq: u64,
}
fn event(item: &DelegationStreamItem) -> serde_json::Result<Event> {
    let name = match item {
        DelegationStreamItem::Page { .. } => "delegation_events",
        DelegationStreamItem::ResyncRequired { .. } => "resync_required",
    };
    // This stream has its own epoch/cursor. It never reuses root transcript SSE IDs.
    Ok(Event::default()
        .event(name)
        .data(serde_json::to_string(item)?))
}
async fn events(
    State(state): State<AppState>,
    Path((id, child)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Query(query): Query<StreamQuery>,
) -> axum::response::Response {
    let subscription = match delegations::subscribe(
        &state.backend,
        &id,
        &child,
        DelegationEventQuery {
            epoch: query.epoch,
            after_seq: query.after_seq,
            limit: 32,
        },
    )
    .await
    {
        Ok(s) => s,
        Err(e) => return HttpReply(e.into_reply()).into_response(),
    };
    let updates = stream::unfold(Some(subscription), |subscription| async move {
        let mut subscription = subscription?;
        let item = subscription.next().await?;
        match event(&item) {
            Ok(event) => Some((Ok::<_, std::convert::Infallible>(event), Some(subscription))),
            Err(_) => Some((
                Ok(Event::default()
                    .event("resync_required")
                    .data("{\"code\":\"delegation_serialization_failed\"}")),
                None,
            )),
        }
    });
    let mut shutdown = state.shutdown;
    Sse::new(
        authorized_events(updates, state.security, headers).take_until(async move {
            if !*shutdown.borrow_and_update() {
                let _ = shutdown.changed().await;
            }
        }),
    )
    .keep_alive(axum::response::sse::KeepAlive::default())
    .into_response()
}
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/api/v1/sessions/{id}/delegations",
            axum::routing::get(roster),
        )
        .route(
            "/api/v1/sessions/{id}/delegations/{child}/events/query",
            axum::routing::post(events_query),
        )
        .route(
            "/api/v1/sessions/{id}/delegations/{child}/cancel",
            axum::routing::post(cancel),
        )
        .route(
            "/api/v1/sessions/{id}/delegations/{child}/events",
            axum::routing::get(events),
        )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delegation_stream_uses_own_event_names_and_strict_cursor_query() {
        let e = event(&DelegationStreamItem::ResyncRequired {
            code: "native_delegation_unavailable",
        })
        .unwrap();
        assert!(format!("{e:?}").contains("resync_required"));
        assert!(serde_json::from_value::<StreamQuery>(
            serde_json::json!({"epoch":"epoch","afterSeq":0,"parentSessionId":"other"})
        )
        .is_err());
    }
}
