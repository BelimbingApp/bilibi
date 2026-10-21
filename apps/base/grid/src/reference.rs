//! The reference tables: non-business stock data owned by Grid so every later
//! slice (query, lenses, page columns, Design Library specimen) has a catalog to
//! run against. Groups, items and movements exercise a one-link, many-links, a
//! chained one-link after a many-link, an enum, decimals, dates, a timestamp and a restrictable
//! field (`reference_items.unit_cost`). A role granted [`READ`] may read them;
//! no product role carries it, the walkthrough fixture's roles do.
use crate::model::{
    Field, FieldType::*, GridContribution, LinkDecl, LinkKind::*, Source, TableDecl,
};
use bilibi_authz::{CapabilityDefinition as C, VocabularyContribution, VOCABULARY};

pub const READ: &str = "grid.reference.view";

static GRID: GridContribution = GridContribution {
    record_fields: &[],
    tables: &[
        TableDecl::new(
            "reference_groups",
            "Reference groups",
            READ,
            Source::tenant("SELECT id, name, region FROM base_grid_ref_groups WHERE tenant_id = $1"),
            "id",
            &[
                Field::new("id", "ID", Integer),
                Field::new("name", "Name", String),
                Field::choice("region", "Region", &["north", "south", "east", "west"]),
            ],
        )
        .label_field("name"),
        TableDecl::new(
            "reference_items",
            "Reference items",
            READ,
            Source::tenant(
                "SELECT id, group_id, name, category, quantity, reorder_level, unit_cost, active FROM base_grid_ref_items WHERE tenant_id = $1",
            ),
            "id",
            &[
                Field::new("id", "ID", Integer),
                Field::new("group_id", "Group", Integer).hidden(),
                Field::new("name", "Name", String),
                Field::choice("category", "Category", &["tools", "parts", "supplies"]),
                Field::new("quantity", "Quantity", Integer),
                Field::new("reorder_level", "Reorder level", Integer),
                Field::new("unit_cost", "Unit cost", Decimal),
                Field::new("active", "Active", Boolean),
            ],
        )
        .label_field("name")
        .record_types(&["reference_item"]),
        TableDecl::new(
            "reference_movements",
            "Reference movements",
            READ,
            Source::tenant("SELECT id, item_id, delta, moved_on, recorded_at FROM base_grid_ref_movements WHERE tenant_id = $1"),
            "id",
            &[
                Field::new("id", "ID", Integer),
                Field::new("item_id", "Item", Integer).hidden(),
                Field::new("delta", "Change", Integer),
                Field::new("moved_on", "Moved on", Date),
                Field::new("recorded_at", "Recorded at", DateTime),
            ],
        )
        .time_field("moved_on"),
    ],
    links: &[
        LinkDecl::on("group", "Group", One, "reference_items", "group_id", "reference_groups", "id"),
        LinkDecl::on("items", "Items", Many, "reference_groups", "id", "reference_items", "group_id"),
        LinkDecl::on("movements", "Movements", Many, "reference_items", "id", "reference_movements", "item_id"),
        LinkDecl::on("item", "Item", One, "reference_movements", "item_id", "reference_items", "id"),
    ],
};
crate::contribute!("base/grid", "grid.reference", &GRID);

/// The walkthrough fixture the lens and page-column pages run on: twelve
/// tenant-1 items in three groups and fourteen months of movements relative to
/// today on the UTC calendar, plus one tenant-2 item. It replaces whatever the
/// reference tables hold; only run it on a disposable database.
pub const FIXTURE: &str = "DELETE FROM base_grid_ref_movements; DELETE FROM base_grid_ref_items; DELETE FROM base_grid_ref_groups;
INSERT INTO base_grid_ref_groups(id,tenant_id,name,region) VALUES (9301,1,'Harbour','north'),(9302,1,'Valley','south'),(9303,1,'Ridge','east'),(9304,2,'Elsewhere','west');
INSERT INTO base_grid_ref_items(id,tenant_id,group_id,name,category,quantity,reorder_level,unit_cost,active) VALUES
 (9311,1,9301,'Anchor bolt','parts',240,50,0.85,true),
 (9312,1,9301,'Bench vise','tools',6,2,184.50,true),
 (9313,1,9301,'Cable ties','supplies',1200,300,0.05,true),
 (9314,1,9302,'Drill bit set','tools',14,4,42.00,false),
 (9315,1,9302,'Epoxy resin','supplies',38,10,27.75,true),
 (9316,1,9302,'Flange gasket','parts',95,20,3.40,true),
 (9317,1,9303,'Grease gun','tools',9,3,64.90,true),
 (9318,1,9303,'Hex nut','parts',3000,500,0.02,false),
 (9319,1,9303,'Insulation tape','supplies',75,25,2.10,true),
 (9320,1,9301,'Jack stand','tools',4,2,129.00,true),
 (9321,1,9302,'Keyed shaft','parts',22,5,58.30,true),
 (9322,1,9303,'Lens cleaner','supplies',0,12,6.60,false),
 (9323,2,9304,'Other tenant','parts',1,1,1.00,true);
INSERT INTO base_grid_ref_movements(id,tenant_id,item_id,delta,moved_on,recorded_at)
SELECT 9400 + row_number() OVER (ORDER BY i.id, k), 1, i.id,
       (((i.id * 7 + k * 13) % 41) - 12)::int,
       (date_trunc('month', current_date AT TIME ZONE 'UTC')::date - make_interval(months => k))::date + ((i.id + k) % 27)::int,
       ((date_trunc('month', current_date AT TIME ZONE 'UTC')::date - make_interval(months => k))::date + ((i.id + k) % 27)::int)::timestamp AT TIME ZONE 'UTC' + make_interval(hours => ((i.id * 5 + k) % 24)::int, mins => ((k * 7) % 60)::int)
FROM base_grid_ref_items i CROSS JOIN generate_series(0, 13) k
WHERE i.tenant_id = 1 AND i.id <> 9322 AND (i.id + k) % 3 <> 0;";

inventory::submit! { bilibi_module_registry::Contribution::new(VOCABULARY, "grid.reference.vocabulary",
bilibi_module_registry::Provenance { module: "base/grid", source: file!(), line: line!() },
&VocabularyContribution {
    domains: &["grid"],
    verbs: &["view"],
    capabilities: &[C::tenant(READ)],
    roles: &[],
    system_principals: &[],
}) }
