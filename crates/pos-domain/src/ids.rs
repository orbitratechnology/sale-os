//! Typed identifiers (ADR-005): UUIDv7, client-generated, canonical lowercase text.
//! Mixing ID kinds is a compile error.

use crate::time::IdGenerator;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {kind}: {value:?}")]
pub struct IdParseError {
    pub kind: &'static str,
    pub value: String,
}

macro_rules! id_type {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
        #[cfg_attr(feature = "specta", derive(specta::Type))]
        #[serde(transparent)]
        pub struct $name(#[cfg_attr(feature = "specta", specta(type = String))] uuid::Uuid);

        impl $name {
            /// Generates a new time-ordered identifier.
            pub fn generate(ids: &dyn IdGenerator) -> Self {
                Self(ids.new_uuid_v7())
            }
            pub const fn from_uuid(u: uuid::Uuid) -> Self {
                Self(u)
            }
            pub const fn as_uuid(&self) -> &uuid::Uuid {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                // uuid's Display is the canonical lowercase hyphenated form.
                std::fmt::Display::fmt(&self.0.as_hyphenated(), f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = IdParseError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                uuid::Uuid::try_parse(s).map(Self).map_err(|_| IdParseError {
                    kind: stringify!($name),
                    value: s.to_owned(),
                })
            }
        }
    )+};
}

id_type!(
    BusinessId,
    LocationId,
    RegisterId,
    UserId,
    SessionId,
    ProductId,
    VariantId,
    BarcodeId,
    CategoryId,
    TaxRateId,
    TaxCategoryId,
    CartId,
    CartLineId,
    SaleId,
    SaleLineId,
    TenderId,
    ReturnId,
    ReturnLineId,
    RefundId,
    MovementId,
    InventoryDocumentId,
    ReceiptId,
    PrintJobId,
    CashSessionId,
    AuditId,
    EventId,
    ScanEventId,
    ConflictId,
    ApprovalId,
    ImportJobId,
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::UuidV7Generator;

    #[test]
    fn display_fromstr_and_serde_round_trip() {
        let id = SaleId::generate(&UuidV7Generator);
        let text = id.to_string();
        assert_eq!(text, text.to_lowercase());
        assert_eq!(text.len(), 36);
        assert_eq!(text.parse::<SaleId>().unwrap(), id);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{text}\""));
        assert_eq!(serde_json::from_str::<SaleId>(&json).unwrap(), id);
    }

    #[test]
    fn uppercase_input_normalises_to_lowercase() {
        let id: VariantId = "0190F0C2-1A2B-7C3D-8E4F-001122334455".parse().unwrap();
        assert_eq!(id.to_string(), "0190f0c2-1a2b-7c3d-8e4f-001122334455");
    }

    #[test]
    fn invalid_id_reports_kind() {
        let err = "not-a-uuid".parse::<ProductId>().unwrap_err();
        assert_eq!(err.kind, "ProductId");
    }

    #[test]
    fn generated_ids_are_v7_and_ordered() {
        let a = EventId::generate(&UuidV7Generator);
        let b = EventId::generate(&UuidV7Generator);
        assert_eq!(a.as_uuid().get_version_num(), 7);
        assert!(a < b);
    }
}
