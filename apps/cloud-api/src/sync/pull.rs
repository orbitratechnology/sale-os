//! What registers pull (E17-T07, SYN-002/008): `GET /v1/sync/changes` (the change feed after
//! a cursor) and `GET /v1/sync/bootstrap` (every master aggregate's current snapshot, paged).
//!
//! The bootstrap is not one database snapshot: an aggregate edited while the pages are read
//! may appear at either version. That is safe because the register then pulls the feed from
//! the `snapshotCursor` taken before the first page, and applies a change only when it is
//! newer than what it holds.

use crate::devices::{DevicePrincipal, internal, pool};
use crate::{AppState, Problem};
use axum::Json;
use axum::extract::{Query, State};
use pos_cloud_db::master::{Kind, snapshot_page};
use pos_cloud_db::sync::{changes_after, feed_position};
use pos_cloud_db::{Principal, TenantTx};
use pos_sync::pull::{
    BootstrapEntity, BootstrapPage, Change, ChangeKind, ChangesResponse, cursor, parse_cursor,
};
use serde::Deserialize;

/// Changes per pull, by default and at most.
const CHANGES_DEFAULT: i64 = 500;
const CHANGES_MAX: i64 = 1000;
/// Aggregates per bootstrap page, by default and at most.
const PAGE_DEFAULT: i64 = 2000;
const PAGE_MAX: i64 = 5000;

#[derive(Deserialize)]
pub struct ChangesQuery {
    cursor: Option<String>,
    limit: Option<i64>,
    /// How many of its events the register still holds that the cloud has not accepted
    /// (E14-T05); recorded for the consolidated report's completeness.
    unsent: Option<i64>,
}

fn principal(me: &DevicePrincipal) -> Principal {
    Principal {
        business_id: me.business_id,
    }
}

pub async fn changes(
    me: DevicePrincipal,
    State(state): State<AppState>,
    Query(q): Query<ChangesQuery>,
) -> Result<Json<ChangesResponse>, Problem> {
    let after = match q.cursor.as_deref() {
        None | Some("") => 0,
        Some(c) => {
            parse_cursor(c).ok_or_else(|| Problem::validation("cursor is not one this server issued"))?
        }
    };
    let limit = q.limit.unwrap_or(CHANGES_DEFAULT).clamp(1, CHANGES_MAX);
    let mut tx = TenantTx::begin(pool(&state)?, &principal(&me))
        .await
        .map_err(internal)?;
    if let Some(n) = q.unsent {
        if n < 0 {
            return Err(Problem::validation("unsent is negative"));
        }
        pos_cloud_db::reports::record_unsent(&mut tx, me.register_id, n)
            .await
            .map_err(internal)?;
    }
    let mut rows = changes_after(&mut tx, after, limit + 1).await.map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    let has_more = rows.len() > usize::try_from(limit).unwrap_or(usize::MAX);
    rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let next = rows.last().map_or(after, |r| r.server_seq);
    let changes = rows
        .into_iter()
        .map(|r| Change {
            server_seq: r.server_seq,
            aggregate_type: r.aggregate_type,
            aggregate_id: r.aggregate_id,
            version: r.version.and_then(|v| u64::try_from(v).ok()),
            kind: match r.change_kind.as_str() {
                "archive" => ChangeKind::Archive,
                "fact" => ChangeKind::Fact,
                _ => ChangeKind::Upsert,
            },
            origin_register_id: r.origin_register,
            event_type: r.event_type,
            schema_version: u32::try_from(r.schema_version).unwrap_or(1),
            snapshot: r.snapshot,
        })
        .collect();
    Ok(Json(ChangesResponse {
        changes,
        next_cursor: cursor(next),
        has_more,
    }))
}

#[derive(Deserialize)]
pub struct BootstrapQuery {
    page: Option<String>,
    size: Option<i64>,
}

/// Where a bootstrap page starts: the snapshot's feed position, the aggregate kind (an index
/// into [`Kind::BOOTSTRAP_ORDER`]) and the last id already sent of that kind.
struct PageToken {
    snapshot: i64,
    kind: usize,
    after: String,
}

impl PageToken {
    fn encode(&self) -> String {
        format!("v1:{}:{}:{}", self.snapshot, self.kind, self.after)
    }

    fn decode(s: &str) -> Option<Self> {
        let mut parts = s.strip_prefix("v1:")?.splitn(3, ':');
        let snapshot = parts.next()?.parse().ok().filter(|n: &i64| *n >= 0)?;
        let kind = parts
            .next()?
            .parse()
            .ok()
            .filter(|k: &usize| *k < Kind::BOOTSTRAP_ORDER.len())?;
        let after = parts.next()?.to_owned();
        Some(Self {
            snapshot,
            kind,
            after,
        })
    }
}

pub async fn bootstrap(
    me: DevicePrincipal,
    State(state): State<AppState>,
    Query(q): Query<BootstrapQuery>,
) -> Result<Json<BootstrapPage>, Problem> {
    let size = q.size.unwrap_or(PAGE_DEFAULT).clamp(1, PAGE_MAX);
    let mut tx = TenantTx::begin(pool(&state)?, &principal(&me))
        .await
        .map_err(internal)?;
    let mut at = match q.page.as_deref() {
        None | Some("") => PageToken {
            snapshot: feed_position(&mut tx).await.map_err(internal)?,
            kind: 0,
            after: String::new(),
        },
        Some(p) => {
            PageToken::decode(p).ok_or_else(|| Problem::validation("page is not one this server issued"))?
        }
    };
    let mut entities = Vec::new();
    let mut next_page = None;
    while at.kind < Kind::BOOTSTRAP_ORDER.len() {
        let kind = Kind::BOOTSTRAP_ORDER[at.kind];
        let room = size - i64::try_from(entities.len()).unwrap_or(i64::MAX);
        let rows = snapshot_page(&mut tx, kind, &at.after, room)
            .await
            .map_err(internal)?;
        let full = i64::try_from(rows.len()).unwrap_or(i64::MAX) == room;
        if let Some((last, ..)) = rows.last() {
            at.after = last.clone();
        }
        entities.extend(rows.into_iter().map(|(id, version, snapshot)| BootstrapEntity {
            aggregate_type: kind.aggregate_type().into(),
            aggregate_id: id,
            event_type: kind.event_type().into(),
            schema_version: 1,
            version: u64::try_from(version).unwrap_or(0),
            snapshot,
        }));
        if full {
            // The page is full: there may be more of this kind.
            next_page = Some(at.encode());
            break;
        }
        at.kind += 1;
        at.after.clear();
    }
    tx.commit().await.map_err(internal)?;
    Ok(Json(BootstrapPage {
        snapshot_cursor: cursor(at.snapshot),
        entities,
        next_page,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_tokens_round_trip_and_reject_anything_else() {
        let t = PageToken {
            snapshot: 9,
            kind: 3,
            after: "0190f0c2-aaaa:with:colons".into(),
        };
        let back = PageToken::decode(&t.encode()).unwrap();
        assert_eq!(
            (back.snapshot, back.kind, back.after.as_str()),
            (9, 3, "0190f0c2-aaaa:with:colons")
        );
        for bad in ["", "v1:9", "v1:9:99:", "v2:9:1:", "v1:-1:0:"] {
            assert!(PageToken::decode(bad).is_none(), "{bad}");
        }
    }
}
