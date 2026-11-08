//! Categories and tax configuration (E06-T05, SET-004, CAT-002).
//!
//! - Categories form a tree at most 3 levels deep, without cycles.
//! - Tax rates have effective dates (`from` inclusive, `to` exclusive; open when absent);
//!   two active rates with the same name may not cover the same day.
//! - A tax category groups 0..n rates; exactly one category is the default.
//! - A rate still used by an active category cannot be archived.

use crate::time::BusinessDate;
use crate::{CategoryId, ErrorCode, HasErrorCode, TaxRateId};

/// Levels of the category tree (a top category is level 1).
pub const MAX_CATEGORY_DEPTH: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("a name is required")]
    NameRequired,
    #[error("the parent category does not exist")]
    ParentNotFound,
    #[error("a category cannot be inside itself or one of its own subcategories")]
    Cycle,
    #[error("categories go at most {MAX_CATEGORY_DEPTH} levels deep")]
    TooDeep,
    #[error("the end date must be after the start date")]
    PeriodBackwards,
    #[error("another active rate called {0:?} covers some of the same days")]
    OverlappingRate(String),
    #[error("the rate is used by {0:?}")]
    RateInUse(Vec<String>),
    #[error("there must be exactly one default tax category")]
    DefaultRequired,
    #[error("that tax rate does not exist or is archived")]
    UnknownRate,
}

impl HasErrorCode for ConfigError {
    fn error_code(&self) -> ErrorCode {
        ErrorCode::ValidationFailed
    }
}

impl ConfigError {
    /// The input at fault, for an inline message.
    pub fn field(&self) -> &'static str {
        match self {
            ConfigError::NameRequired | ConfigError::OverlappingRate(_) => "name",
            ConfigError::ParentNotFound | ConfigError::Cycle | ConfigError::TooDeep => "parent",
            ConfigError::PeriodBackwards => "period",
            ConfigError::RateInUse(_) => "categories",
            ConfigError::DefaultRequired => "default",
            ConfigError::UnknownRate => "rates",
        }
    }
}

pub fn clean_name(name: &str) -> Result<String, ConfigError> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 80 {
        return Err(ConfigError::NameRequired);
    }
    Ok(n.to_owned())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CategoryNode {
    pub id: CategoryId,
    pub parent: Option<CategoryId>,
}

/// Checks putting category `id` under `parent` (a new category or a move), against the
/// active tree `all` (which may already contain `id`).
pub fn check_category_parent(
    all: &[CategoryNode],
    id: CategoryId,
    parent: Option<CategoryId>,
) -> Result<(), ConfigError> {
    let parent_of = |c: CategoryId| all.iter().find(|n| n.id == c).map(|n| n.parent);
    // Levels above, counting the parent itself.
    let mut above = 0;
    let mut cursor = parent;
    while let Some(p) = cursor {
        if p == id {
            return Err(ConfigError::Cycle);
        }
        above += 1;
        if above > all.len() {
            return Err(ConfigError::Cycle); // a cycle elsewhere in stored data
        }
        cursor = parent_of(p).ok_or(ConfigError::ParentNotFound)?;
    }
    // Levels of `id`'s own subtree (1 for a leaf).
    fn height(all: &[CategoryNode], id: CategoryId, guard: usize) -> usize {
        if guard == 0 {
            return usize::MAX / 2;
        }
        1 + all
            .iter()
            .filter(|n| n.parent == Some(id))
            .map(|n| height(all, n.id, guard - 1))
            .max()
            .unwrap_or(0)
    }
    if above + height(all, id, all.len() + 1) > MAX_CATEGORY_DEPTH {
        return Err(ConfigError::TooDeep);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RatePeriod {
    pub id: TaxRateId,
    pub name: String,
    pub from: Option<BusinessDate>,
    pub to: Option<BusinessDate>,
}

/// `[from, to)` with open ends.
fn overlaps(a: &RatePeriod, b: &RatePeriod) -> bool {
    let starts_before_b_ends = match (a.from, b.to) {
        (Some(f), Some(t)) => f < t,
        _ => true,
    };
    let b_starts_before_a_ends = match (b.from, a.to) {
        (Some(f), Some(t)) => f < t,
        _ => true,
    };
    starts_before_b_ends && b_starts_before_a_ends
}

/// Checks a rate's period against the other **active** rates.
pub fn check_rate_period(active: &[RatePeriod], rate: &RatePeriod) -> Result<(), ConfigError> {
    if let (Some(f), Some(t)) = (rate.from, rate.to)
        && f >= t
    {
        return Err(ConfigError::PeriodBackwards);
    }
    if let Some(other) = active
        .iter()
        .find(|o| o.id != rate.id && o.name.eq_ignore_ascii_case(&rate.name) && overlaps(o, rate))
    {
        return Err(ConfigError::OverlappingRate(other.name.clone()));
    }
    Ok(())
}

/// A rate may be archived only when no active category uses it: `users` are the active
/// categories `(name, rates)`.
pub fn check_rate_archivable(rate: TaxRateId, users: &[(String, Vec<TaxRateId>)]) -> Result<(), ConfigError> {
    let using: Vec<String> = users
        .iter()
        .filter(|(_, rates)| rates.contains(&rate))
        .map(|(n, _)| n.clone())
        .collect();
    if using.is_empty() {
        Ok(())
    } else {
        Err(ConfigError::RateInUse(using))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UuidV7Generator;

    fn id() -> CategoryId {
        CategoryId::generate(&UuidV7Generator)
    }

    #[test]
    fn three_levels_and_no_cycles() {
        let (food, drinks, hot, tea) = (id(), id(), id(), id());
        let mut all = vec![
            CategoryNode {
                id: food,
                parent: None,
            },
            CategoryNode {
                id: drinks,
                parent: Some(food),
            },
            CategoryNode {
                id: hot,
                parent: Some(drinks),
            },
        ];
        // A fourth level is refused; a sibling at level 3 is fine.
        assert_eq!(
            check_category_parent(&all, tea, Some(hot)),
            Err(ConfigError::TooDeep)
        );
        assert_eq!(check_category_parent(&all, tea, Some(drinks)), Ok(()));
        // Moving "drinks" (two levels) under a level-2 category would make four.
        let snacks = id();
        all.push(CategoryNode {
            id: snacks,
            parent: Some(food),
        });
        assert_eq!(
            check_category_parent(&all, drinks, Some(snacks)),
            Err(ConfigError::TooDeep)
        );
        // Under itself or its own descendant: a cycle.
        assert_eq!(
            check_category_parent(&all, food, Some(hot)),
            Err(ConfigError::Cycle)
        );
        assert_eq!(
            check_category_parent(&all, food, Some(food)),
            Err(ConfigError::Cycle)
        );
        assert_eq!(
            check_category_parent(&all, tea, Some(id())),
            Err(ConfigError::ParentNotFound)
        );
        // Moving "hot" to the top is fine.
        assert_eq!(check_category_parent(&all, hot, None), Ok(()));
    }

    fn rate(name: &str, from: Option<&str>, to: Option<&str>) -> RatePeriod {
        RatePeriod {
            id: TaxRateId::generate(&UuidV7Generator),
            name: name.into(),
            from: from.map(|d| d.parse().unwrap()),
            to: to.map(|d| d.parse().unwrap()),
        }
    }

    #[test]
    fn rates_with_the_same_name_cannot_cover_the_same_day() {
        let current = rate("VAT", None, Some("2026-04-01"));
        let active = vec![current.clone()];
        // The new rate starts the day the old one ends: fine (to is exclusive).
        assert_eq!(
            check_rate_period(&active, &rate("VAT", Some("2026-04-01"), None)),
            Ok(())
        );
        assert_eq!(
            check_rate_period(&active, &rate("vat", Some("2026-03-31"), None)),
            Err(ConfigError::OverlappingRate("VAT".into()))
        );
        assert_eq!(check_rate_period(&active, &rate("Reduced", None, None)), Ok(()));
        assert_eq!(
            check_rate_period(&active, &rate("X", Some("2026-05-01"), Some("2026-05-01"))),
            Err(ConfigError::PeriodBackwards)
        );
        // Editing a rate is not an overlap with itself.
        assert_eq!(check_rate_period(&active, &current), Ok(()));
    }

    #[test]
    fn a_used_rate_cannot_be_archived() {
        let (vat, zero) = (
            TaxRateId::generate(&UuidV7Generator),
            TaxRateId::generate(&UuidV7Generator),
        );
        let users = vec![
            ("Standard".to_owned(), vec![vat]),
            ("Mixed".to_owned(), vec![vat, zero]),
        ];
        assert_eq!(
            check_rate_archivable(vat, &users),
            Err(ConfigError::RateInUse(vec!["Standard".into(), "Mixed".into()]))
        );
        assert_eq!(
            check_rate_archivable(TaxRateId::generate(&UuidV7Generator), &users),
            Ok(())
        );
    }
}
