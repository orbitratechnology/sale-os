//! Sync conflicts (E18, SYN-010, ADR-011, AT-07): what differs between this register's edit
//! and the cloud's version, field by field, and resolving it.
//!
//! A conflict happens when this register edited master data from a version the cloud had
//! already moved past (`VERSION`), or gave a product a barcode or SKU another product holds
//! in the cloud (`BARCODE_TAKEN`, `SKU_TAKEN`). Nothing is overwritten silently: the cloud
//! kept its version, and the till keeps using its own until someone resolves the conflict.
//!
//! Resolving a version conflict starts from the cloud's version and, per differing field,
//! keeps the local value, the cloud's, or a custom one. The result becomes the aggregate's
//! next version (cloud version + 1), is applied here, and travels as an event based on the
//! cloud's version, so the cloud accepts it.

use crate::audit::{AuditEntry, SYNC_CONFLICT_RESOLVED};
use crate::context::{AppError, CommandContext};
use crate::queries::QueryError;
use crate::records::AggregateRef;
use crate::write::{LocalStore, OutboxDraft, WriteSet};
use pos_domain::{ErrorCode, Permission};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;

/// An open conflict as stored (`sync_conflicts`).
#[derive(Clone, Debug, PartialEq)]
pub struct ConflictRecord {
    pub conflict_id: String,
    pub aggregate_type: String,
    pub aggregate_id: String,
    /// The edit this register sent (its newest, when several conflicted).
    pub local: Value,
    /// The cloud's version: of this aggregate, or of the product holding the barcode or SKU.
    pub server: Value,
    pub local_base_version: u64,
    pub server_version: u64,
    /// `VERSION`, `BARCODE_TAKEN` or `SKU_TAKEN`.
    pub reason: String,
    pub created_at: pos_domain::Timestamp,
}

/// Conflict reads (implemented by `pos-local-db`).
#[async_trait::async_trait]
pub trait ConflictQueries: Send + Sync {
    async fn open_conflicts(&self) -> Result<Vec<ConflictRecord>, QueryError>;
    async fn open_conflict(&self, conflict_id: &str) -> Result<Option<ConflictRecord>, QueryError>;
    /// The local version of an aggregate (`None` if this register does not have it).
    async fn local_version(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
    ) -> Result<Option<u64>, QueryError>;
}

/// One field that differs.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldDiff {
    /// Where it is: `name`, `variants/<id>/price`...
    pub field: String,
    /// What to call it on screen.
    pub label: String,
    pub local: Value,
    pub server: Value,
}

/// What to keep for one field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "keep", content = "value")]
pub enum Choice {
    Local,
    Server,
    Custom(Value),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldChoice {
    pub field: String,
    pub choice: Choice,
}

/// The aggregate types the cloud versions, with their event type.
fn event_type(aggregate_type: &str) -> Option<(&'static str, &'static str)> {
    Some(match aggregate_type {
        "product" => ("product", "product.upserted"),
        "category" => ("category", "category.upserted"),
        "tax_rate" => ("tax_rate", "tax_rate.upserted"),
        "tax_category" => ("tax_category", "tax_category.upserted"),
        "setting" => ("setting", "setting.updated"),
        "staff_user" => ("staff_user", "staff.upserted"),
        "business" => ("business", "business.updated"),
        "location" => ("location", "location.upserted"),
        "register" => ("register", "register.upserted"),
        _ => return None,
    })
}

/// The fields compared per aggregate type (besides products' variants), with their labels.
fn fields(aggregate_type: &str) -> &'static [(&'static str, &'static str)] {
    match aggregate_type {
        "product" => &[
            ("name", "Name"),
            ("description", "Description"),
            ("categoryId", "Category"),
            ("taxCategoryId", "Tax category"),
            ("quickSelect", "Quick-select button"),
            ("archived", "Archived"),
        ],
        "category" => &[
            ("name", "Name"),
            ("parentId", "Parent"),
            ("sortOrder", "Order"),
            ("archived", "Archived"),
        ],
        "tax_rate" => &[
            ("name", "Name"),
            ("rate", "Rate"),
            ("effectiveFrom", "From"),
            ("effectiveTo", "To"),
            ("archived", "Archived"),
        ],
        "tax_category" => &[
            ("name", "Name"),
            ("isDefault", "Default"),
            ("rateIds", "Rates"),
            ("archived", "Archived"),
        ],
        "setting" => &[("value", "Value")],
        "staff_user" => &[
            ("displayName", "Name"),
            ("role", "Role"),
            ("active", "Active"),
            ("mustChangePin", "Must change PIN"),
            ("pinHash", "PIN"),
        ],
        "business" => &[
            ("name", "Business name"),
            ("legalName", "Legal name"),
            ("taxRegistrationNo", "Tax registration number"),
            ("address", "Address"),
            ("phone", "Phone"),
            ("email", "Email"),
        ],
        "location" => &[("name", "Name"), ("address", "Address"), ("archived", "Archived")],
        "register" => &[
            ("name", "Name"),
            ("receiptPrefix", "Receipt prefix"),
            ("retired", "Retired"),
        ],
        _ => &[],
    }
}

const VARIANT_FIELDS: &[(&str, &str)] = &[
    ("sku", "SKU"),
    ("price", "Price"),
    ("cost", "Cost"),
    ("lowStockThreshold", "Low-stock alert"),
    ("trackStock", "Track stock"),
    ("archived", "Archived"),
];

fn variant_label(v: &Value) -> String {
    let options: Vec<&str> = v["options"]
        .as_object()
        .map(|o| o.values().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if options.is_empty() {
        String::new()
    } else {
        format!(" ({})", options.join(" / "))
    }
}

fn variants(p: &Value) -> Vec<&Value> {
    p["variants"]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn find_variant<'a>(p: &'a Value, id: &str) -> Option<&'a Value> {
    variants(p).into_iter().find(|v| v["id"].as_str() == Some(id))
}

/// The barcodes of a variant as a sorted list of values (how they are compared and shown).
fn barcode_values(v: &Value) -> Value {
    let mut codes: Vec<String> = v["barcodes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|b| b["value"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    codes.sort();
    json!(codes)
}

/// A PIN hash is never shown: only whether it changed.
fn shown(field: &str, v: &Value, other: &Value) -> Value {
    if field == "pinHash" {
        return json!(if v == other { "unchanged" } else { "changed" });
    }
    v.clone()
}

/// The fields that differ between this register's edit and the cloud's version (E18-T01).
pub fn diff(aggregate_type: &str, local: &Value, server: &Value) -> Vec<FieldDiff> {
    let mut out = Vec::new();
    for (field, label) in fields(aggregate_type) {
        let (l, s) = (&local[*field], &server[*field]);
        if l != s {
            out.push(FieldDiff {
                field: (*field).into(),
                label: (*label).into(),
                local: shown(field, l, s),
                server: shown(field, s, l),
            });
        }
    }
    if aggregate_type == "product" {
        let mut ids: Vec<String> = Vec::new();
        for v in variants(local).into_iter().chain(variants(server)) {
            if let Some(id) = v["id"].as_str()
                && !ids.iter().any(|x| x == id)
            {
                ids.push(id.to_owned());
            }
        }
        for id in ids {
            let (l, s) = (find_variant(local, &id), find_variant(server, &id));
            let suffix = variant_label(l.or(s).unwrap_or(&Value::Null));
            match (l, s) {
                (Some(l), Some(s)) => {
                    for (field, label) in VARIANT_FIELDS {
                        if l[*field] != s[*field] {
                            out.push(FieldDiff {
                                field: format!("variants/{id}/{field}"),
                                label: format!("{label}{suffix}"),
                                local: l[*field].clone(),
                                server: s[*field].clone(),
                            });
                        }
                    }
                    let (lb, sb) = (barcode_values(l), barcode_values(s));
                    if lb != sb {
                        out.push(FieldDiff {
                            field: format!("variants/{id}/barcodes"),
                            label: format!("Barcodes{suffix}"),
                            local: lb,
                            server: sb,
                        });
                    }
                }
                (l, s) => out.push(FieldDiff {
                    field: format!("variants/{id}"),
                    label: format!("Variant{suffix}"),
                    local: json!(l.is_some()),
                    server: json!(s.is_some()),
                }),
            }
        }
    }
    out
}

/// The cloud's version with the chosen fields replaced. Every differing field needs a choice.
pub fn merge(
    aggregate_type: &str,
    local: &Value,
    server: &Value,
    choices: &[FieldChoice],
) -> Result<Value, String> {
    let differing = diff(aggregate_type, local, server);
    let mut merged = server.clone();
    for d in &differing {
        let Some(c) = choices.iter().find(|c| c.field == d.field) else {
            return Err(format!("no choice for {}", d.label));
        };
        let parts: Vec<&str> = d.field.split('/').collect();
        match (&c.choice, parts.as_slice()) {
            (Choice::Server, _) => {}
            (Choice::Custom(_), [_, _, "barcodes"] | [_, _]) | (Choice::Custom(_), ["pinHash"]) => {
                return Err(format!("{} can only be kept from one side", d.label));
            }
            (choice, [field]) => {
                merged[*field] = match choice {
                    Choice::Local => local[*field].clone(),
                    Choice::Custom(v) => v.clone(),
                    Choice::Server => unreachable!(),
                };
            }
            (Choice::Local, ["variants", id]) => {
                // A variant only one side has: keep this register's (add it, or drop it).
                let mut list: Vec<Value> = variants(&merged)
                    .into_iter()
                    .filter(|v| v["id"].as_str() != Some(id))
                    .cloned()
                    .collect();
                if let Some(v) = find_variant(local, id) {
                    list.push(v.clone());
                }
                merged["variants"] = json!(list);
            }
            (choice, ["variants", id, field]) => {
                let source = match choice {
                    Choice::Local => find_variant(local, id).map(|v| {
                        if *field == "barcodes" {
                            v["barcodes"].clone()
                        } else {
                            v[*field].clone()
                        }
                    }),
                    Choice::Custom(v) => Some(v.clone()),
                    Choice::Server => None,
                };
                if let (Some(value), Some(list)) = (source, merged["variants"].as_array_mut())
                    && let Some(v) = list.iter_mut().find(|v| v["id"].as_str() == Some(id))
                {
                    v[*field] = value;
                }
            }
            _ => return Err(format!("cannot resolve {}", d.field)),
        }
    }
    Ok(merged)
}

/// What the conflict screens show.
#[derive(Clone, Debug, PartialEq)]
pub struct ConflictView {
    pub record: ConflictRecord,
    /// The aggregate's name, for the list.
    pub title: String,
    pub fields: Vec<FieldDiff>,
    /// Barcode or SKU conflicts: the values the other product holds.
    pub taken: Vec<String>,
}

/// Active barcodes and SKUs of a product payload.
fn identifiers(p: &Value) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut codes = BTreeSet::new();
    let mut skus = BTreeSet::new();
    if p["archived"].as_bool() == Some(true) {
        return (codes, skus);
    }
    for v in variants(p)
        .into_iter()
        .filter(|v| v["archived"].as_bool() != Some(true))
    {
        for b in v["barcodes"].as_array().into_iter().flatten() {
            if let Some(c) = b["value"].as_str() {
                codes.insert(c.to_owned());
            }
        }
        if let Some(s) = v["sku"].as_str() {
            skus.insert(s.to_lowercase());
        }
    }
    (codes, skus)
}

/// The barcodes and SKUs both products hold.
fn clash(ours: &Value, theirs: &Value) -> (BTreeSet<String>, BTreeSet<String>) {
    let (oc, os) = identifiers(ours);
    let (tc, ts) = identifiers(theirs);
    (
        oc.intersection(&tc).cloned().collect(),
        os.intersection(&ts).cloned().collect(),
    )
}

/// `p` without those barcodes and SKUs.
fn without(p: &Value, codes: &BTreeSet<String>, skus: &BTreeSet<String>) -> Value {
    let mut p = p.clone();
    if let Some(list) = p["variants"].as_array_mut() {
        for v in list {
            if let Some(b) = v["barcodes"].as_array_mut() {
                b.retain(|b| !b["value"].as_str().is_some_and(|c| codes.contains(c)));
            }
            if v["sku"]
                .as_str()
                .is_some_and(|s| skus.contains(&s.to_lowercase()))
            {
                v["sku"] = Value::Null;
            }
        }
    }
    p
}

pub struct ConflictService {
    pub store: Arc<dyn LocalStore>,
    pub conflicts: Arc<dyn ConflictQueries>,
}

impl ConflictService {
    /// Open conflicts with what differs (ConflictResolve).
    pub async fn list(&self, ctx: &CommandContext) -> Result<Vec<ConflictView>, AppError> {
        ctx.authorize(Permission::ConflictResolve)?;
        Ok(self
            .conflicts
            .open_conflicts()
            .await?
            .into_iter()
            .map(|c| {
                let title = c.local["name"]
                    .as_str()
                    .or(c.local["displayName"].as_str())
                    .or(c.local["key"].as_str())
                    .unwrap_or(&c.aggregate_id)
                    .to_owned();
                let (fields, taken) = if c.reason == "VERSION" {
                    (diff(&c.aggregate_type, &c.local, &c.server), Vec::new())
                } else {
                    let (codes, skus) = clash(&c.local, &c.server);
                    (Vec::new(), codes.into_iter().chain(skus).collect())
                };
                ConflictView {
                    record: c,
                    title,
                    fields,
                    taken,
                }
            })
            .collect())
    }

    async fn open(&self, conflict_id: &str) -> Result<ConflictRecord, AppError> {
        self.conflicts.open_conflict(conflict_id).await?.ok_or_else(|| {
            AppError::Validation(
                "this conflict is already resolved".into(),
                ErrorCode::ConflictVersion,
            )
        })
    }

    /// One aggregate's next version, applied here and sent based on the cloud's version.
    async fn write(
        &self,
        ctx: &CommandContext,
        aggregate_type: &str,
        aggregate_id: &str,
        payload: &Value,
        base: u64,
    ) -> Result<(crate::WriteOp, OutboxDraft, AggregateRef), AppError> {
        let (aggregate_type, event_type) = event_type(aggregate_type)
            .ok_or_else(|| AppError::Internal(format!("{aggregate_type} is not versioned")))?;
        let mut payload = payload.clone();
        payload["version"] = json!(base + 1);
        let local = self.conflicts.local_version(aggregate_type, aggregate_id).await?;
        let op = crate::sync::inbound_master(event_type, 1, &payload, local, ctx.register.business_id)
            .map_err(|e| AppError::Validation(e.0, ErrorCode::ValidationFailed))?;
        let aggregate = AggregateRef {
            aggregate_type,
            aggregate_id: aggregate_id.to_owned(),
        };
        let draft = OutboxDraft::new(
            ctx.ids.as_ref(),
            event_type,
            1,
            aggregate.clone(),
            Some(base),
            &payload,
        )?;
        Ok((op, draft, aggregate))
    }

    /// Resolves a version conflict with a choice per differing field (E18-T02, AT-07).
    pub async fn resolve(
        &self,
        ctx: &CommandContext,
        conflict_id: &str,
        choices: Vec<FieldChoice>,
    ) -> Result<(), AppError> {
        ctx.authorize(Permission::ConflictResolve)?;
        let c = self.open(conflict_id).await?;
        if c.reason != "VERSION" {
            return Err(AppError::Validation(
                "a barcode or SKU conflict is resolved by removing or moving it".into(),
                ErrorCode::ValidationFailed,
            ));
        }
        let merged = merge(&c.aggregate_type, &c.local, &c.server, &choices)
            .map_err(|e| AppError::Validation(e, ErrorCode::ValidationFailed))?;
        let (op, draft, aggregate) = self
            .write(ctx, &c.aggregate_type, &c.aggregate_id, &merged, c.server_version)
            .await?;
        // The audit keeps the choices, never a PIN hash.
        let kept: Vec<Value> = choices
            .iter()
            .map(|ch| match &ch.choice {
                Choice::Custom(v) if ch.field != "pinHash" => {
                    json!({"field": ch.field, "keep": "custom", "value": v})
                }
                Choice::Custom(_) | Choice::Local => json!({"field": ch.field, "keep": "local"}),
                Choice::Server => json!({"field": ch.field, "keep": "server"}),
            })
            .collect();
        let resolution = json!({"choices": kept, "serverVersion": c.server_version});
        let entry = AuditEntry::new(
            ctx.ids.as_ref(),
            SYNC_CONFLICT_RESOLVED,
            aggregate.aggregate_type,
            &c.aggregate_id,
        )
        .with_metadata(
            json!({"conflictId": c.conflict_id, "choices": kept, "serverVersion": c.server_version}),
        )
        .linked_to(draft.event_id);
        let set = WriteSet::builder(ctx.write_context())
            .push(op)
            .resolving(aggregate)
            .outbox(draft)
            .audit(entry)
            .resolve_conflict(c.conflict_id.clone(), resolution)
            .build()?;
        self.store.apply(set).await?;
        Ok(())
    }

    /// Resolves a barcode or SKU conflict (E18-T02): `move_here = false` removes the clashing
    /// codes from this product; `true` takes them from the product that holds them in the
    /// cloud (two products change).
    pub async fn resolve_identifiers(
        &self,
        ctx: &CommandContext,
        conflict_id: &str,
        move_here: bool,
    ) -> Result<(), AppError> {
        ctx.authorize(Permission::ConflictResolve)?;
        let c = self.open(conflict_id).await?;
        if c.reason == "VERSION" {
            return Err(AppError::Validation(
                "this is not a barcode or SKU conflict".into(),
                ErrorCode::ValidationFailed,
            ));
        }
        let (codes, skus) = clash(&c.local, &c.server);
        let ids = ctx.ids.as_ref();
        let mut set = WriteSet::builder(ctx.write_context());
        let mut events = Vec::new();
        if move_here {
            // The other product first: the cloud frees the codes before this one claims them.
            let owner = c.server["id"].as_str().unwrap_or_default().to_owned();
            let freed = without(&c.server, &codes, &skus);
            let (op, draft, aggregate) = self
                .write(ctx, "product", &owner, &freed, c.server_version)
                .await?;
            events.push(draft.event_id);
            set = set.push(op).resolving(aggregate).outbox(draft);
            let (op, draft, aggregate) = self
                .write(ctx, "product", &c.aggregate_id, &c.local, c.local_base_version)
                .await?;
            events.push(draft.event_id);
            set = set.push(op).resolving(aggregate).outbox(draft);
        } else {
            let kept = without(&c.local, &codes, &skus);
            let (op, draft, aggregate) = self
                .write(ctx, "product", &c.aggregate_id, &kept, c.local_base_version)
                .await?;
            events.push(draft.event_id);
            set = set.push(op).resolving(aggregate).outbox(draft);
        }
        let taken: Vec<String> = codes.into_iter().chain(skus).collect();
        let resolution = json!({"move": move_here, "values": taken});
        let entry = AuditEntry::new(ids, SYNC_CONFLICT_RESOLVED, "product", &c.aggregate_id)
            .with_metadata(
                json!({"conflictId": c.conflict_id, "reason": c.reason, "move": move_here, "values": taken}),
            )
            .linked_to(events[events.len() - 1]);
        let set = set
            .audit(entry)
            .resolve_conflict(c.conflict_id.clone(), resolution)
            .build()?;
        self.store.apply(set).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn product(price: i64, name: &str, barcodes: &[&str]) -> Value {
        let codes: Vec<Value> = barcodes
            .iter()
            .map(|c| json!({"id": format!("b-{c}"), "value": c, "raw": c, "symbology": "ean13", "qualifier": "primary", "packQty": 1000}))
            .collect();
        json!({"id": "p1", "name": name, "description": null, "categoryId": null, "productType": "stock", "unit": "ea",
               "taxCategoryId": "t1", "quickSelect": false, "archived": false, "version": 3,
               "variants": [{"id": "v1", "isDefault": true, "sku": "COLA", "options": {"Size": "330ml"}, "price": price,
                             "cost": null, "trackStock": true, "lowStockThreshold": null, "scanQty": 1000,
                             "barcodes": codes, "archived": false}]})
    }

    #[test]
    fn a_price_conflict_shows_local_against_server_per_field() {
        let local = product(150, "Cola", &["111"]);
        let server = product(140, "Cola Zero", &["111", "222"]);
        let d = diff("product", &local, &server);
        let summary: Vec<(&str, &str, &Value, &Value)> = d
            .iter()
            .map(|f| (f.field.as_str(), f.label.as_str(), &f.local, &f.server))
            .collect();
        assert_eq!(
            summary,
            [
                ("name", "Name", &json!("Cola"), &json!("Cola Zero")),
                ("variants/v1/price", "Price (330ml)", &json!(150), &json!(140)),
                (
                    "variants/v1/barcodes",
                    "Barcodes (330ml)",
                    &json!(["111"]),
                    &json!(["111", "222"])
                ),
            ]
        );
    }

    #[test]
    fn merging_starts_from_the_server_and_applies_each_choice() {
        let local = product(150, "Cola", &["111"]);
        let server = product(140, "Cola Zero", &["111", "222"]);
        let choices = vec![
            FieldChoice {
                field: "name".into(),
                choice: Choice::Custom(json!("Cola (new)")),
            },
            FieldChoice {
                field: "variants/v1/price".into(),
                choice: Choice::Local,
            },
            FieldChoice {
                field: "variants/v1/barcodes".into(),
                choice: Choice::Server,
            },
        ];
        let m = merge("product", &local, &server, &choices).unwrap();
        assert_eq!(m["name"], "Cola (new)");
        assert_eq!(m["variants"][0]["price"], 150);
        assert_eq!(barcode_values(&m["variants"][0]), json!(["111", "222"]));
        // Every differing field needs a choice.
        assert!(merge("product", &local, &server, &choices[..2]).is_err());
    }

    #[test]
    fn a_pin_change_is_shown_but_never_the_hash() {
        let local = json!({"displayName": "Ade", "role": "cashier", "active": true, "mustChangePin": false, "pinHash": "$argon2id$a"});
        let server = json!({"displayName": "Ade", "role": "manager", "active": true, "mustChangePin": false, "pinHash": "$argon2id$b"});
        let d = diff("staff_user", &local, &server);
        assert_eq!(d.len(), 2);
        assert_eq!(
            (d[1].local.clone(), d[1].server.clone()),
            (json!("changed"), json!("changed"))
        );
        assert!(!serde_json::to_string(&d).unwrap().contains("argon2"));
    }

    #[test]
    fn clashing_barcodes_and_skus_are_found_and_removed() {
        let ours = product(150, "Cola", &["111", "333"]);
        let mut theirs = product(140, "Other", &["111"]);
        theirs["id"] = json!("p2");
        theirs["variants"][0]["sku"] = json!("cola");
        let (codes, skus) = clash(&ours, &theirs);
        assert_eq!(
            (
                codes.iter().map(String::as_str).collect::<Vec<_>>(),
                skus.iter().map(String::as_str).collect::<Vec<_>>()
            ),
            (vec!["111"], vec!["cola"])
        );
        let freed = without(&ours, &codes, &skus);
        assert_eq!(barcode_values(&freed["variants"][0]), json!(["333"]));
        assert_eq!(freed["variants"][0]["sku"], Value::Null);
    }
}
