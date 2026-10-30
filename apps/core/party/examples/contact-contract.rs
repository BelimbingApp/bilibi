//! Deterministic inspectable contract specimen, not a persisted fixture or test.
use bilibi_party::{
    Affiliation, AffiliationId, AffiliationInput, EffectiveDates, OrganisationId, PartyId, Person,
    PersonId, PersonInput, PersonKind, Relationship, RelationshipId, RelationshipInput,
    RelationshipRoleId, OWNERSHIP,
};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let person_id = PersonId::new(701)?;
    let own = OrganisationId::new(1)?;
    let vendor = OrganisationId::new(2)?;
    let other = OrganisationId::new(3)?;
    let dates = EffectiveDates::new(Some("2026-10-14".to_owned().try_into()?), None)?;
    let person = Person {
        code: "morgan_contact".into(),
        id: person_id,
        version: 1,
        identity: PersonInput {
            name: "Morgan Contact".into(),
            kind: PersonKind::Human,
            email: None,
            phone: None,
        },
    };
    let affiliations = [(801, vendor, "Sales director"), (802, other, "Adviser")]
        .into_iter()
        .map(|(id, organisation_id, title)| {
            Ok(Affiliation {
                id: AffiliationId::new(id)?,
                version: 1,
                affiliation: AffiliationInput {
                    person_id,
                    organisation_id,
                    title: Some(title.into()),
                    work_email: Some("morgan@example.test".into()),
                    work_phone: Some("+60 12 345 6789".into()),
                    dates: dates.clone(),
                },
            })
        })
        .collect::<Result<Vec<_>, bilibi_party_ids::InvalidId>>()?;
    let relationships = [(901, 501), (902, 502)]
        .into_iter()
        .map(|(id, role_id)| {
            Ok(Relationship {
                id: RelationshipId::new(id)?,
                version: 1,
                relationship: RelationshipInput {
                    source_organisation_id: own,
                    target: PartyId::Organisation(vendor),
                    role_id: RelationshipRoleId::new(role_id)?,
                    dates: dates.clone(),
                },
            })
        })
        .collect::<Result<Vec<_>, bilibi_party_ids::InvalidId>>()?;
    let owners: Vec<_> = OWNERSHIP
        .iter()
        .map(|owner| {
            json!({
                "entity": owner.entity, "module": owner.module,
                "table": owner.table, "operation_prefix": owner.operation_prefix,
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "specimen_only": true,
            "people_installed": false,
            "person": person,
            "affiliations": affiliations,
            "roles": [
                {"id": 501, "code": "vendor", "name": "Vendor", "inverse_name": "Customer"},
                {"id": 502, "code": "customer", "name": "Customer", "inverse_name": "Vendor"}
            ],
            "relationships": relationships,
            "person_relationship_target": PartyId::Person(person_id),
            "owners": owners,
        }))?
    );
    Ok(())
}
