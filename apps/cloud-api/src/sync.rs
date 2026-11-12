//! Sync ingestion (E17-T04, SYN-005/006, ADR-010): `POST /v1/sync/events` and
//! `GET /v1/sync/ack`.
//!
//! A register's events are applied in its `localSequence` order, each in its own transaction
//! that first locks the register's `register_sync_state` row, so two pushes from one register
//! never interleave. Per event:
//!
//! 1. Already in the inbox: the same envelope gets `duplicate` with the first outcome; a
//!    different envelope under that id is refused (`EVENT_ID_REUSED`, an integrity alert).
//! 2. Not `ack + 1`: `out_of_order` with the expected sequence; the batch stops.
//! 3. An unknown type or a newer schema version: `rejected: EVENT_VERSION_UNSUPPORTED`, not
//!    recorded and not acknowledged, so the client keeps the event and sends it again once
//!    the server supports it; the batch stops.
//! 4. Otherwise the applier runs. Applied, conflict and rejected outcomes are recorded in the
//!    inbox and the ack moves to this event, in the same commit; a rejection first undoes the
//!    applier's own writes. A transient failure rolls everything back and stops the batch.
//!
//! The response's `ackSequence` is read after the loop: everything up to it is committed.

use crate::devices::{DevicePrincipal, internal, pool, same_tenant};
use crate::{AppState, Problem};
use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use pos_cloud_db::sync::{self as db, NewInboxEntry};
use pos_cloud_db::{Principal, TenantTx};
use pos_domain::ErrorCode;
use pos_sync::envelope::{MAX_PUSH_BYTES, MAX_PUSH_EVENTS};
use pos_sync::{
    AckResponse, EventEnvelope, EventResult, Payload, PayloadError, PushRequest, PushResponse, ResultStatus,
    codes,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::sync::Arc;

pub fn routes() -> Router<AppState> {
    Router::new()
        // Room above the 2 MB contract limit, so an oversized push gets a problem, not a reset.
        .route(
            "/sync/events",
            post(push).layer(DefaultBodyLimit::max(MAX_PUSH_BYTES + 64 * 1024)),
        )
        .route("/sync/ack", get(ack))
}

// ---------- appliers ----------

/// What applying an event did.
#[derive(Clone, Debug, PartialEq)]
pub enum ApplyOutcome {
    Applied {
        /// The change-feed row written for it, if any.
        server_seq: Option<i64>,
        /// Master data: the aggregate's version now.
        new_version: Option<u64>,
    },
    /// Master data edited from an old version (or a uniqueness clash): nothing changed.
    Conflict {
        server_version: u64,
        server_snapshot: Value,
        /// One of the conflict codes in [`pos_sync::codes`].
        reason: &'static str,
    },
}

#[derive(Debug)]
pub enum ApplyError {
    /// Permanent: recorded, acknowledged, and shown in the client's repair queue.
    Rejected { code: &'static str, detail: String },
    /// Try again later: nothing is recorded and the batch stops here.
    Transient { code: &'static str, detail: String },
}

impl From<sqlx::Error> for ApplyError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!(error = %e, "applier database error");
        ApplyError::Transient {
            code: codes::SERVER_BUSY,
            detail: "database error".into(),
        }
    }
}

/// Applies one event type to the cloud tables (E17-T05 facts, E17-T06 master data).
#[async_trait]
pub trait EventApplier: Send + Sync {
    fn event_type(&self) -> &'static str;
    /// The (upcast) schema versions it handles.
    fn supported_versions(&self) -> RangeInclusive<u32>;
    /// Runs inside the event's transaction; must not commit.
    async fn apply(
        &self,
        tx: &mut TenantTx<'_>,
        me: &DevicePrincipal,
        env: &EventEnvelope,
        payload: &Payload,
    ) -> Result<ApplyOutcome, ApplyError>;
}

/// The appliers by event type.
#[derive(Clone, Default)]
pub struct Appliers(HashMap<&'static str, Arc<dyn EventApplier>>);

impl Appliers {
    /// The production set. Event types without an applier yet get `transient:
    /// APPLIER_UNAVAILABLE`, so clients keep them and retry after a deploy.
    pub fn standard() -> Self {
        Self::default()
    }

    pub fn with(mut self, a: Arc<dyn EventApplier>) -> Self {
        self.0.insert(a.event_type(), a);
        self
    }

    fn get(&self, event_type: &str) -> Option<&Arc<dyn EventApplier>> {
        self.0.get(event_type)
    }
}

// ---------- the push ----------

async fn push(
    me: DevicePrincipal,
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<PushResponse>, Problem> {
    if body.len() > MAX_PUSH_BYTES {
        return Err(Problem::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::ValidationFailed,
            "Push too large",
            Some(format!("at most {MAX_PUSH_BYTES} bytes per push")),
        ));
    }
    let req: PushRequest = serde_json::from_slice(&body).map_err(|e| Problem::validation(e.to_string()))?;
    if req.events.len() > MAX_PUSH_EVENTS {
        return Err(Problem::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::ValidationFailed,
            "Push too large",
            Some(format!("at most {MAX_PUSH_EVENTS} events per push")),
        ));
    }
    same_tenant(&me, None, Some(req.register_id))?;
    for e in &req.events {
        same_tenant(&me, Some(e.business_id), Some(e.register_id))?;
    }
    let pool = pool(&state)?;
    let principal = Principal {
        business_id: me.business_id,
    };
    let mut results = Vec::with_capacity(req.events.len());
    for env in &req.events {
        let (result, stop) = match ingest_one(pool, &principal, &me, &state.appliers, env).await {
            Ok(step) => step,
            Err(e) => {
                tracing::error!(error = %e, event_id = %env.event_id, "sync ingestion failed");
                let r = EventResult::new(env.event_id, env.local_sequence, ResultStatus::Transient)
                    .with_code(codes::SERVER_BUSY, None);
                (r, true)
            }
        };
        results.push(result);
        if stop {
            break;
        }
    }
    let mut tx = TenantTx::begin(pool, &principal).await.map_err(internal)?;
    let ack = db::ack(&mut tx, me.register_id).await.map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(Json(PushResponse {
        ack_sequence: u64::try_from(ack).unwrap_or(0),
        server_time: now_rfc3339(),
        results,
    }))
}

/// One event, in its own transaction. Returns its result and whether the batch stops.
async fn ingest_one(
    pool: &sqlx::PgPool,
    principal: &Principal,
    me: &DevicePrincipal,
    appliers: &Appliers,
    env: &EventEnvelope,
) -> Result<(EventResult, bool), sqlx::Error> {
    let hash = envelope_hash(env);
    let seq = i64::try_from(env.local_sequence).unwrap_or(i64::MAX);
    let result = |status| EventResult::new(env.event_id, env.local_sequence, status);
    let mut tx = TenantTx::begin(pool, principal).await?;
    let ack = db::lock_register(&mut tx, me.register_id).await?;

    if let Some(entry) = db::inbox_entry(&mut tx, env.event_id).await? {
        tx.rollback().await?;
        let same = entry.payload_sha256 == hash
            && entry.register_id == env.register_id
            && entry.local_sequence == seq;
        if !same {
            tracing::error!(
                alert = "sync.event_id_reused",
                event_id = %env.event_id,
                register_id = %me.register_id,
                device_id = %me.device_id,
                "an event id was reused for a different event"
            );
            let r = result(ResultStatus::Rejected).with_code(
                codes::EVENT_ID_REUSED,
                Some("This event id was already used for a different event".into()),
            );
            return Ok((r, true));
        }
        let mut r: EventResult =
            serde_json::from_value(entry.result).map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
        r.original_status = Some(r.status);
        r.status = ResultStatus::Duplicate;
        return Ok((r, false));
    }
    if seq != ack + 1 {
        tx.rollback().await?;
        let mut r = result(ResultStatus::OutOfOrder);
        r.expected_sequence = Some(u64::try_from(ack + 1).unwrap_or(1));
        return Ok((r, true));
    }

    // Recorded outcomes need a valid timestamp to store; an envelope without one is invalid.
    let occurred_at = chrono::DateTime::parse_from_rfc3339(&env.occurred_at).map(|t| t.to_utc());
    let checked = match (
        &occurred_at,
        pos_sync::parse_payload(&env.event_type, env.schema_version, &env.payload),
    ) {
        (_, Err(PayloadError::UnknownType(_) | PayloadError::UnsupportedVersion { .. })) => {
            tx.rollback().await?;
            let r = result(ResultStatus::Rejected).with_code(
                codes::EVENT_VERSION_UNSUPPORTED,
                Some(format!(
                    "{} v{} is not supported by this server yet",
                    env.event_type, env.schema_version
                )),
            );
            return Ok((r, true));
        }
        (Err(_), _) => Err(ApplyError::Rejected {
            code: codes::EVENT_INVALID,
            detail: "occurredAt is not an RFC 3339 time".into(),
        }),
        (_, Err(e @ PayloadError::Invalid { .. })) => Err(ApplyError::Rejected {
            code: codes::EVENT_INVALID,
            detail: e.to_string(),
        }),
        (Ok(_), Ok(payload)) => envelope_consistent(env).map(|()| payload),
    };
    let outcome = match checked {
        Err(e) => Err(e),
        Ok(payload) => {
            let version = pos_sync::events::current_version(&env.event_type).unwrap_or(env.schema_version);
            match appliers.get(&env.event_type) {
                Some(a) if a.supported_versions().contains(&version) => {
                    tx.savepoint().await?;
                    let o = a.apply(&mut tx, me, env, &payload).await;
                    if matches!(o, Err(ApplyError::Rejected { .. })) {
                        tx.rollback_to_savepoint().await?;
                    }
                    tx.release_savepoint().await?;
                    o
                }
                _ => Err(ApplyError::Transient {
                    code: codes::APPLIER_UNAVAILABLE,
                    detail: format!("{} cannot be applied by this server yet", env.event_type),
                }),
            }
        }
    };
    let (r, status) = match outcome {
        Err(ApplyError::Transient { code, detail }) => {
            tx.rollback().await?;
            return Ok((
                result(ResultStatus::Transient).with_code(code, Some(detail)),
                true,
            ));
        }
        Ok(ApplyOutcome::Applied {
            server_seq,
            new_version,
        }) => {
            let mut r = result(ResultStatus::Applied);
            r.server_seq = server_seq;
            r.server_version = new_version;
            (r, "applied")
        }
        Ok(ApplyOutcome::Conflict {
            server_version,
            server_snapshot,
            reason,
        }) => {
            let mut r = result(ResultStatus::Conflict).with_code(reason, None);
            r.server_version = Some(server_version);
            r.server_snapshot = Some(server_snapshot);
            (r, "conflict")
        }
        Err(ApplyError::Rejected { code, detail }) => (
            result(ResultStatus::Rejected).with_code(code, Some(detail)),
            "rejected",
        ),
    };
    let stored = serde_json::to_value(&r).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    db::insert_inbox(
        &mut tx,
        &NewInboxEntry {
            event_id: env.event_id,
            register_id: env.register_id,
            local_sequence: seq,
            event_type: &env.event_type,
            schema_version: i32::try_from(env.schema_version).unwrap_or(i32::MAX),
            // Rejected for a bad time: stored with the receipt time instead.
            occurred_at: occurred_at.unwrap_or_else(|_| chrono::Utc::now()),
            payload: &env.payload,
            payload_sha256: &hash,
            result_status: status,
            result: &stored,
        },
    )
    .await?;
    db::set_ack(&mut tx, me.register_id, seq).await?;
    tx.commit().await?;
    Ok((r, false))
}

/// Envelope rules the payload schema cannot see: master events carry `baseVersion`, facts
/// never do (ADR-011).
fn envelope_consistent(env: &EventEnvelope) -> Result<(), ApplyError> {
    let master = pos_sync::events::is_master(&env.event_type);
    if master == env.base_version.is_some() {
        Ok(())
    } else {
        Err(ApplyError::Rejected {
            code: codes::EVENT_INVALID,
            detail: if master {
                "a master-data event needs baseVersion".into()
            } else {
                "a fact event has no baseVersion".into()
            },
        })
    }
}

/// SHA-256 of the envelope as canonical JSON (keys sorted at every level), so the same event
/// hashes the same however its JSON was laid out.
pub fn envelope_hash(env: &EventEnvelope) -> Vec<u8> {
    fn canonical(v: &Value, out: &mut String) {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String(k.clone()).to_string());
                    out.push(':');
                    canonical(&m[k], out);
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    canonical(x, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut s = String::new();
    canonical(&serde_json::to_value(env).unwrap_or(Value::Null), &mut s);
    Sha256::digest(s.as_bytes()).to_vec()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

// ---------- the ack ----------

async fn ack(me: DevicePrincipal, State(state): State<AppState>) -> Result<Json<AckResponse>, Problem> {
    let pool = pool(&state)?;
    let mut tx = TenantTx::begin(
        pool,
        &Principal {
            business_id: me.business_id,
        },
    )
    .await
    .map_err(internal)?;
    let ack = db::ack(&mut tx, me.register_id).await.map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(Json(AckResponse {
        register_id: me.register_id,
        ack_sequence: u64::try_from(ack).unwrap_or(0),
        server_time: now_rfc3339(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env(payload: Value) -> EventEnvelope {
        EventEnvelope {
            event_id: uuid::Uuid::nil(),
            business_id: uuid::Uuid::nil(),
            location_id: uuid::Uuid::nil(),
            register_id: uuid::Uuid::nil(),
            local_sequence: 1,
            event_type: "audit.recorded".into(),
            schema_version: 1,
            aggregate_type: "audit".into(),
            aggregate_id: "x".into(),
            base_version: None,
            occurred_at: "2026-11-05T09:14:03.120Z".into(),
            payload,
        }
    }

    #[test]
    fn the_hash_ignores_key_order_but_not_content() {
        let a = env(serde_json::from_str(r#"{"a":1,"b":{"c":2,"d":[1,2]}}"#).unwrap());
        let b = env(serde_json::from_str(r#"{"b":{"d":[1,2],"c":2},"a":1}"#).unwrap());
        let c = env(json!({"a": 1, "b": {"c": 2, "d": [2, 1]}}));
        assert_eq!(envelope_hash(&a), envelope_hash(&b));
        assert_ne!(envelope_hash(&a), envelope_hash(&c));
        let mut d = a.clone();
        d.local_sequence = 2;
        assert_ne!(envelope_hash(&a), envelope_hash(&d));
    }

    #[test]
    fn master_events_need_a_base_version_and_facts_refuse_one() {
        let mut e = env(json!({}));
        assert!(envelope_consistent(&e).is_ok());
        e.base_version = Some(0);
        assert!(envelope_consistent(&e).is_err());
        e.event_type = "product.upserted".into();
        assert!(envelope_consistent(&e).is_ok());
        e.base_version = None;
        assert!(envelope_consistent(&e).is_err());
    }
}
