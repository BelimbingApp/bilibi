//! Approved Core party contract, independent of People and login accounts.
//! This module publishes DTOs and schema ownership, not persistence or proofs.
//! See CONTRACT.md for owner APIs, scoped foreign keys and migration obligations.
pub use bilibi_party_ids::{
    AffiliationId, OrganisationId, PartyId, PersonId, RelationshipId, RelationshipRoleId,
};
use serde::{Deserialize, Serialize};

pub mod dates;
/// JSON Schema definitions for owner operation registration; never authorization.
pub const INPUT_SCHEMAS: &str = include_str!("../schema.json");

pub use dates::{CalendarDate, EffectiveDates};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonKind {
    Human,
    Agent,
}

/// Personal contacts are distinct from the work contacts of each affiliation.
/// No HR status/type/number, implicit account or access grant is representable.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PersonInput {
    pub name: String,
    pub kind: PersonKind,
    pub email: Option<String>,
    pub phone: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Person {
    pub id: PersonId,
    pub code: String,
    pub version: i64,
    pub identity: PersonInput,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrganisationStatus {
    Active,
    Suspended,
    Pending,
    Archived,
}
/// Company remains the owning module and public route name. Organisation is
/// its party identity; this is not a second competing company table.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Organisation {
    pub id: OrganisationId,
    pub name: String,
    pub status: OrganisationStatus,
    pub version: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AffiliationInput {
    pub person_id: PersonId,
    pub organisation_id: OrganisationId,
    pub title: Option<String>,
    pub work_email: Option<String>,
    pub work_phone: Option<String>,
    pub dates: EffectiveDates,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Affiliation {
    pub id: AffiliationId,
    pub version: i64,
    pub affiliation: AffiliationInput,
}

/// Labels read from the source company's perspective. A role carries no
/// authorization capability. Renames preserve its ID; retirement preserves links.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RelationshipRoleInput {
    pub code: String,
    pub name: String,
    pub inverse_name: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RelationshipRole {
    pub id: RelationshipRoleId,
    pub version: i64,
    pub role: RelationshipRoleInput,
    pub retired_on: Option<CalendarDate>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RelationshipInput {
    pub source_organisation_id: OrganisationId,
    pub target: PartyId,
    pub role_id: RelationshipRoleId,
    pub dates: EffectiveDates,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Relationship {
    pub id: RelationshipId,
    pub version: i64,
    pub relationship: RelationshipInput,
}

/// Stable schema/API owners for downstream slices; no SQL is installed by K0.
#[derive(Clone, Copy, Debug)]
pub struct Ownership {
    pub entity: &'static str,
    pub module: &'static str,
    pub table: &'static str,
    pub operation_prefix: &'static str,
}
pub const OWNERSHIP: &[Ownership] = &[
    Ownership {
        entity: "organisation",
        module: "core/company",
        table: "companies",
        operation_prefix: "core.company",
    },
    Ownership {
        entity: "person",
        module: "core/person",
        table: "persons",
        operation_prefix: "core.person",
    },
    Ownership {
        entity: "affiliation",
        module: "core/affiliation",
        table: "person_affiliations",
        operation_prefix: "core.affiliation",
    },
    Ownership {
        entity: "relationship_role",
        module: "core/company",
        table: "company_relationship_roles",
        operation_prefix: "core.company.relationship_role",
    },
    Ownership {
        entity: "relationship",
        module: "core/company",
        table: "company_relationships",
        operation_prefix: "core.company.relationship",
    },
];

pub const DEFAULT_RELATIONSHIP_ROLES: &[(&str, &str, &str)] = &[
    ("vendor", "Vendor", "Customer"),
    ("customer", "Customer", "Vendor"),
    ("prospect", "Prospect", "Prospective vendor"),
    ("partner", "Partner", "Partner"),
];

inventory::submit! {
    bilibi_module_registry::Module {
        id: "core/party", layer: bilibi_module_registry::Layer::Core,
        dependencies: &["base/module_registry", "base/party_ids"], migrations: &[], services: &[],
    }
}

/// The short suggestion for a new party code. Explicit codes are not subject to
/// this limit. The suffix is 1 for the unsuffixed candidate, 2 for `_2`, etc.
/// Mirrors Bilimbi Core.Party's shared identity vocabulary.
#[derive(Clone, Copy)]
pub enum CodeKind {
    Company,
    Person,
}

pub fn default_code(name: &str, kind: CodeKind, suffix: u64) -> String {
    let cleaned = name.replace("(M)", " ").replace("(m)", " ");
    let slug = slug_code(&cleaned);
    let words: Vec<_> = slug
        .split('_')
        .filter(|word| !word.is_empty())
        .filter(|word| match kind {
            CodeKind::Company => !matches!(
                *word,
                "sdn"
                    | "bhd"
                    | "berhad"
                    | "pte"
                    | "ltd"
                    | "limited"
                    | "plt"
                    | "inc"
                    | "incorporated"
                    | "llc"
                    | "corp"
                    | "corporation"
                    | "co"
                    | "company"
                    | "plc"
                    | "llp"
            ),
            CodeKind::Person => !matches!(*word, "bin" | "binti" | "al" | "ap" | "so" | "do"),
        })
        .collect();
    let fallback = match kind {
        CodeKind::Company => "company",
        CodeKind::Person => "person",
    };
    let suffix = if suffix <= 1 {
        String::new()
    } else {
        format!("_{suffix}")
    };
    let capacity = 12usize.saturating_sub(suffix.len());
    let mut base = String::new();
    for word in words {
        if base.is_empty() {
            base.push_str(&word[..word.len().min(capacity)]);
        } else if base.len() + 1 + word.len() <= capacity {
            base.push('_');
            base.push_str(word);
        } else {
            break;
        }
    }
    if base.is_empty() {
        base.push_str(&fallback[..fallback.len().min(capacity)]);
    }
    format!("{base}{suffix}")
}

/// `BlbStr::code`: Belimbing's slug of a name, kept so a code generated here
/// matches one generated by the source. Dashes become the separator, `@` the
/// word `at`, other punctuation is removed (not separated), Latin accents are
/// stripped and anything still non-ASCII is dropped.
pub fn slug_code(name: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for ch in name.chars() {
        let ch = match ch {
            '-' | '_' => '_',
            '@' => {
                push_separated(&mut slug, &mut separator);
                slug.push_str("at");
                separator = true;
                continue;
            }
            ch if ch.is_whitespace() => '_',
            ch => strip_accent(ch),
        };
        if ch == '_' {
            separator = !slug.is_empty();
        } else if ch.is_ascii_alphanumeric() {
            push_separated(&mut slug, &mut separator);
            slug.push(ch.to_ascii_lowercase());
        }
    }
    slug.chars().take(255).collect()
}
fn push_separated(slug: &mut String, separator: &mut bool) {
    if *separator && !slug.is_empty() {
        slug.push('_');
    }
    *separator = false;
}
/// Latin-1 and Latin Extended-A letters to their base letter; others unchanged.
fn strip_accent(ch: char) -> char {
    const TABLE: &[(&str, char)] = &[
        ("ÀÁÂÃÄÅĀĂĄ", 'A'),
        ("àáâãäåāăą", 'a'),
        ("ÇĆĈĊČ", 'C'),
        ("çćĉċč", 'c'),
        ("ĎĐ", 'D'),
        ("ďđ", 'd'),
        ("ÈÉÊËĒĔĖĘĚ", 'E'),
        ("èéêëēĕėęě", 'e'),
        ("ĜĞĠĢ", 'G'),
        ("ĝğġģ", 'g'),
        ("ĤĦ", 'H'),
        ("ĥħ", 'h'),
        ("ÌÍÎÏĨĪĬĮİ", 'I'),
        ("ìíîïĩīĭįı", 'i'),
        ("Ĵ", 'J'),
        ("ĵ", 'j'),
        ("Ķ", 'K'),
        ("ķ", 'k'),
        ("ĹĻĽĿŁ", 'L'),
        ("ĺļľŀł", 'l'),
        ("ÑŃŅŇ", 'N'),
        ("ñńņň", 'n'),
        ("ÒÓÔÕÖØŌŎŐ", 'O'),
        ("òóôõöøōŏő", 'o'),
        ("ŔŖŘ", 'R'),
        ("ŕŗř", 'r'),
        ("ŚŜŞŠ", 'S'),
        ("śŝşš", 's'),
        ("ŢŤŦ", 'T'),
        ("ţťŧ", 't'),
        ("ÙÚÛÜŨŪŬŮŰŲ", 'U'),
        ("ùúûüũūŭůűų", 'u'),
        ("Ŵ", 'W'),
        ("ŵ", 'w'),
        ("ÝŶŸ", 'Y'),
        ("ýÿŷ", 'y'),
        ("ŹŻŽ", 'Z'),
        ("źżž", 'z'),
    ];
    TABLE
        .iter()
        .find(|(accented, _)| accented.contains(ch))
        .map_or(ch, |(_, base)| *base)
}
