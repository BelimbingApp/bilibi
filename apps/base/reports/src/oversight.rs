//! Saved oversight questions compose Grid's sealed fan-out, never child owner
//! reads. Ordinary reports keep their existing path.
use crate::*;
use bilibi_grid::{OversightData, OversightSpec};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChildOutcome {
    pub tenant: i64,
    pub name: String,
    pub grant_version: i64,
    pub outcome: &'static str,
}

impl Run {
    pub(crate) fn oversight_status(&self) -> Option<String> {
        if !self.report.definition.oversight {
            return None;
        }
        let excluded: Vec<_> = self
            .oversight
            .iter()
            .filter(|c| c.outcome != "ok")
            .collect();
        Some(if excluded.is_empty() {
            format!("Read-only parent oversight: {} consenting children included. No child record links.",self.oversight.len())
        } else {
            format!(
                "Partial: {} of {} consenting children not included. {}.",
                excluded.len(),
                self.oversight.len(),
                excluded
                    .iter()
                    .map(|c| format!("{}: {}", c.name, c.outcome.replace('_', " ")))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        })
    }
}

impl Reports {
    pub(super) async fn catalog(
        &self,
        scope: &Scope,
        oversight: bool,
        rows: bool,
    ) -> Outcome<Catalog> {
        if oversight {
            self.oversight
                .builder_catalog(scope, rows)
                .await
                .map_err(grid)
        } else {
            let client = self.db.get().await.map_err(Error::from)?;
            Catalog::for_scope(&**client, scope)
                .await
                .map_err(|e| ReportError::Database(e.into()))
        }
    }

    pub(super) async fn run_oversight(
        &self,
        scope: &Scope,
        report: Report,
        input: &RunInput,
    ) -> Outcome<Run> {
        if input.background {
            return Err(ReportError::Refused("Oversight results are live reads. Download them directly; they cannot be retained or scheduled in the parent.".into()));
        }
        let as_rows = report.kind == Kind::Rows || input.as_rows;
        let catalog = self.catalog(scope, true, as_rows).await?;
        let root = catalog
            .table(&report.record_type)
            .ok_or(ReportError::Unreadable)?;
        let definition = &report.definition;
        let mut filters = Vec::new();
        for (filter, answer) in definition
            .filters
            .iter()
            .enumerate()
            .map(|(i, f)| (f, input.answers.get(&i)))
            .chain(input.drill.iter().map(|f| (f, None)))
        {
            let column = catalog
                .resolve(root, &filter.column)
                .map_err(|_| ReportError::HiddenFilter(filter.column.clone()))?;
            filters.extend(
                typed_filter(filter, answer, &column)
                    .map_err(|why| ReportError::Invalid(field_errors("filters", why)))?,
            );
        }
        // Unlike ordinary display pruning, hidden grouping/sort inputs must
        // refuse: a forged spec cannot silently become a different question.
        for spec in definition
            .groupings
            .iter()
            .map(|g| g.column.as_str())
            .chain(definition.sort.iter().map(|s| s.column.as_str()))
        {
            catalog
                .resolve(root, spec)
                .map_err(|_| ReportError::HiddenFilter(spec.into()))?;
        }
        let client = self.db.get().await.map_err(Error::from)?;
        let timezone = self
            .display
            .display(&**client, scope, None, "en")
            .await?
            .timezone;
        let page_size = self
            .setting(&**client, scope, contributions::PAGE_SIZE_KEY)
            .await?
            .clamp(1, 100);
        let max_groups = self
            .setting(&**client, scope, contributions::MAX_GROUPS_KEY)
            .await?;
        drop(client);
        let bounds = self.bounds(scope, false).await?;
        let proof = self.oversight.prove(scope).await.map_err(grid)?;
        let mut left_out = Vec::new();
        let mut keep = |spec: &str| {
            let resolved = catalog.resolve(root, spec).ok();
            if resolved.is_none() && !left_out.iter().any(|s| s == spec) {
                left_out.push(spec.to_owned());
            }
            resolved
        };
        let child_column = ColumnInfo {
            spec: "_oversight_child".into(),
            label: "Child tenant".into(),
            ty: "string",
        };
        let (result, outcomes) = if as_rows {
            let specs = if report.kind == Kind::Rows {
                definition.columns.clone()
            } else {
                behind(root, definition)
            };
            let columns: Vec<_> = specs.iter().filter_map(|s| keep(s)).collect();
            let counts = self
                .oversight
                .read(
                    &proof,
                    &report.record_type,
                    &OversightSpec::Rows {
                        page: PageSpec {
                            bounds,
                            filters: filters.clone(),
                            sorts: vec![],
                            timezone: timezone.clone(),
                            offset: 0,
                            limit: 1,
                        },
                        columns: vec![],
                        offsets: BTreeMap::new(),
                    },
                )
                .await
                .map_err(grid)?;
            let mut skip = input.page.saturating_mul(page_size);
            let mut remaining = page_size;
            let mut selected = BTreeMap::new();
            for child in &counts.children {
                if let Some(OversightData::Rows { total, .. }) = &child.data {
                    let count = usize::try_from(*total).map_err(|_| Error::Internal)?;
                    let offset = skip.min(count);
                    skip -= offset;
                    let take = remaining.min(count - offset);
                    if take > 0 {
                        selected.insert(child.tenant, (offset, take));
                        remaining -= take;
                    }
                }
            }
            let read = self
                .oversight
                .read(
                    &proof,
                    &report.record_type,
                    &OversightSpec::Rows {
                        page: PageSpec {
                            bounds,
                            filters: filters.clone(),
                            sorts: definition.sorts(),
                            timezone: timezone.clone(),
                            offset: 0,
                            limit: page_size,
                        },
                        columns: columns.iter().map(|c| c.spec.clone()).collect(),
                        offsets: selected
                            .iter()
                            .map(|(id, (offset, _))| (*id, *offset))
                            .collect(),
                    },
                )
                .await
                .map_err(grid)?;
            if read.children.len() != counts.children.len()
                || read.children.iter().zip(&counts.children).any(|(a, b)| {
                    a.tenant != b.tenant
                        || a.grant_version != b.grant_version
                        || a.outcome != b.outcome
                        || match (&a.data, &b.data) {
                            (
                                Some(OversightData::Rows { total: a, .. }),
                                Some(OversightData::Rows { total: b, .. }),
                            ) => a != b,
                            _ => false,
                        }
                })
            {
                return Err(ReportError::Refused(
                    "Oversight access or record counts changed while paging. Refresh the report."
                        .into(),
                ));
            }
            let mut rows = Vec::new();
            let mut total = 0;
            let mut outcomes = Vec::new();
            for child in read.children {
                outcomes.push(ChildOutcome {
                    tenant: child.tenant,
                    name: child.name.clone(),
                    grant_version: child.grant_version,
                    outcome: child.outcome,
                });
                if let Some(OversightData::Rows {
                    projection,
                    total: count,
                }) = child.data
                {
                    total += count;
                    for row in projection
                        .rows
                        .into_iter()
                        .take(selected.get(&child.tenant).map_or(0, |(_, take)| *take))
                    {
                        let mut cells = vec![json!(child.name)];
                        cells.extend(
                            columns
                                .iter()
                                .map(|c| row.cells.get(&c.id).cloned().unwrap_or(Value::Null)),
                        );
                        rows.push((json!([child.tenant, row.key]), cells));
                    }
                }
            }
            let totals = if definition.totals && !input.as_rows {
                let numeric: Vec<_> = columns
                    .iter()
                    .filter(|c| c.ty.number() && c.spec != root.key())
                    .collect();
                let mut sums = BTreeMap::new();
                for chunk in numeric.chunks(bilibi_grid::MAX_MEASURES) {
                    let totals = self
                        .oversight
                        .read(
                            &proof,
                            &report.record_type,
                            &OversightSpec::RowsSummary(SummarySpec {
                                bounds,
                                filters: filters.clone(),
                                groupings: vec![],
                                measures: chunk
                                    .iter()
                                    .map(|c| bilibi_grid::Measure {
                                        column: c.spec.clone(),
                                        agg: bilibi_grid::Agg::Sum,
                                    })
                                    .collect(),
                                timezone: timezone.clone(),
                                max_groups: 1,
                            }),
                        )
                        .await
                        .map_err(grid)?;
                    // Never attach totals from a different included cohort.
                    if totals.children.len() != outcomes.len()
                        || totals.children.iter().zip(&outcomes).any(|(a, b)| {
                            a.tenant != b.tenant
                                || a.grant_version != b.grant_version
                                || a.outcome != b.outcome
                        })
                    {
                        return Err(ReportError::Refused("Oversight access or availability changed while reading totals. Refresh the report.".into()));
                    }
                    for (c, v) in chunk
                        .iter()
                        .zip(totals.total.ok_or(Error::Internal)?.values)
                    {
                        sums.insert(c.spec.clone(), v);
                    }
                }
                Some(
                    std::iter::once(Value::Null)
                        .chain(
                            columns
                                .iter()
                                .map(|c| sums.remove(&c.spec).unwrap_or(Value::Null)),
                        )
                        .collect(),
                )
            } else {
                None
            };
            let mut shown = vec![child_column];
            shown.extend(columns.iter().map(info));
            (
                RunResult::Rows {
                    columns: shown,
                    rows,
                    pages: vec![],
                    total,
                    page: input.page,
                    page_size,
                    totals,
                },
                outcomes,
            )
        } else {
            let groupings = definition.groupings();
            let measures: Vec<_> = definition
                .measures()
                .into_iter()
                .filter(|m| keep(&m.column).is_some())
                .collect();
            let read = self
                .oversight
                .read(
                    &proof,
                    &report.record_type,
                    &OversightSpec::Summary(SummarySpec {
                        bounds,
                        filters,
                        groupings: groupings.clone(),
                        measures: measures.clone(),
                        timezone: timezone.clone(),
                        max_groups,
                    }),
                )
                .await
                .map_err(grid)?;
            let total = read.total.ok_or(Error::Internal)?;
            let mut lines = Vec::new();
            let mut outcomes = Vec::new();
            for child in read.children {
                outcomes.push(ChildOutcome {
                    tenant: child.tenant,
                    name: child.name.clone(),
                    grant_version: child.grant_version,
                    outcome: child.outcome,
                });
                if let Some(OversightData::Summary(summary)) = child.data {
                    for line in summary.lines {
                        let mut keys = vec![json!(child.name)];
                        keys.extend(line.keys);
                        lines.push((keys, line.subtotal, line.rows, line.values));
                    }
                    let mut keys = vec![json!(child.name)];
                    keys.extend(groupings.iter().map(|_| Value::Null));
                    lines.push((
                        keys,
                        !groupings.is_empty(),
                        summary.total.rows,
                        summary.total.values,
                    ));
                }
            }
            let mut groups = vec![(child_column, None)];
            groups.extend(groupings.iter().filter_map(|g| {
                catalog
                    .resolve(root, &g.column)
                    .ok()
                    .map(|c| (info(&c), g.bucket.map(|b| b.key())))
            }));
            let measures = measures
                .iter()
                .filter_map(|m| {
                    catalog
                        .resolve(root, &m.column)
                        .ok()
                        .map(|c| (info(&c), m.agg.key()))
                })
                .collect();
            (
                RunResult::Summary {
                    groupings: groups,
                    measures,
                    lines,
                    total: (total.rows, total.values),
                    chart: None,
                },
                outcomes,
            )
        };
        Ok(Run {
            report,
            oversight: outcomes,
            left_out,
            timezone,
            result,
        })
    }
}
