//! Mirrors Bilimbi UI Web DesignLibraryLive; interactive lists use FlexTable.
use crate::state::{self, State};
use askama::Template;
use bilibi_datetime::{Format, Precision};
use bilibi_ui::{
    Badge, BadgeKind, Column, EmptyState, FlexRow, FlexTable, Html, IconButton, IconKind,
    PageCounts, Pagination, Row, Table, TableEmpty,
};

#[derive(Template)]
#[template(path = "library.html")]
pub struct View<'a> {
    pub state: &'a State,
}
pub fn render(fragment: impl Template) -> askama::Result<Html> {
    Html::render(&fragment)
}
macro_rules! sections {
    ($($name:ident:$path:literal),* $(,)?) => {$(const _:() = {
        #[derive(Template)]
        #[template(path=$path)]
        struct Section<'a,'s>(&'a View<'s>);
        impl<'a,'s> std::ops::Deref for Section<'a,'s>{type Target=View<'s>;fn deref(&self)->&Self::Target{self.0}}
        impl View<'_>{fn $name(&self)->askama::Result<Html>{Html::render(&Section(self))}}
    };)*};
}
sections! {
theme: "design_library/theme.html",
component_page_structure: "design_library/component_page_structure.html",
card_component_tile_controls: "design_library/card_component_tile_controls.html",
card_component_record_link: "design_library/card_component_record_link.html",
card_component_split_handle: "design_library/card_component_split_handle.html",
card_component_page: "design_library/card_component_page.html",
card_component_header: "design_library/card_component_header.html",
part_7: "design_library/part_7.html",
part_8: "design_library/part_8.html",
card_component_section_heading: "design_library/card_component_section_heading.html",
part_10: "design_library/part_10.html",
part_11: "design_library/part_11.html",
card_component_inline_long_text: "design_library/card_component_inline_long_text.html",
card_component_inline_choice: "design_library/card_component_inline_choice.html",
component_navigation_links: "design_library/component_navigation_links.html",
card_design_library_navigation: "design_library/card_design_library_navigation.html",
card_component_tabs: "design_library/card_component_tabs.html",
card_component_back_link: "design_library/card_component_back_link.html",
card_component_action_link: "design_library/card_component_action_link.html",
component_actions: "design_library/component_actions.html",
card_component_icon_button: "design_library/card_component_icon_button.html",
component_inputs: "design_library/component_inputs.html",
card_design_library_input_guidance: "design_library/card_design_library_input_guidance.html",
card_text_and_long_form_inputs: "design_library/card_text_and_long_form_inputs.html",
card_choice_controls: "design_library/card_choice_controls.html",
card_component_multi_select: "design_library/card_component_multi_select.html",
card_component_radio_group_states: "design_library/card_component_radio_group_states.html",
card_date_time_and_secret_inputs: "design_library/card_date_time_and_secret_inputs.html",
card_component_secret_input: "design_library/card_component_secret_input.html",
card_design_library_input_live_state: "design_library/card_design_library_input_live_state.html",
card_component_input_states: "design_library/card_component_input_states.html",
card_component_input_variants: "design_library/card_component_input_variants.html",
card_component_combobox: "design_library/card_component_combobox.html",
card_component_file_input: "design_library/card_component_file_input.html",
component_interaction_patterns: "design_library/component_interaction_patterns.html",
card_component_inline_edit: "design_library/card_component_inline_edit.html",
component_feedback_states: "design_library/component_feedback_states.html",
card_stored: "design_library/card_stored.html",
card_refused: "design_library/card_refused.html",
card_nothing_yet: "design_library/card_nothing_yet.html",
card_nothing_matched: "design_library/card_nothing_matched.html",
card_not_permitted: "design_library/card_not_permitted.html",
component_overlays: "design_library/component_overlays.html",
card_component_modal: "design_library/card_component_modal.html",
part_43: "design_library/part_43.html",
part_44: "design_library/part_44.html",
card_component_confirm_dialog: "design_library/card_component_confirm_dialog.html",
component_data_display: "design_library/component_data_display.html",
card_component_table: "design_library/card_component_table.html",
card_sample_flex_plain_card: "design_library/card_sample_flex_plain_card.html",
card_component_card: "design_library/card_component_card.html",
card_company_profile: "design_library/card_company_profile.html",
card_surface_51: "design_library/card_surface_51.html",
card_component_list: "design_library/card_component_list.html",
card_component_stat_strip: "design_library/card_component_stat_strip.html",
card_component_datetime: "design_library/card_component_datetime.html",
component_composite_patterns: "design_library/component_composite_patterns.html",
card_surface_56: "design_library/card_surface_56.html",
card_component_filter_toolbar: "design_library/card_component_filter_toolbar.html",
card_component_code_input: "design_library/card_component_code_input.html",
card_component_period_filter: "design_library/card_component_period_filter.html",
components: "design_library/components_catalog.html",
graphic: "design_library/graphic.html",
card_spec_t01: "design_library/card_spec_t01.html",
card_spec_d01: "design_library/card_spec_d01.html",
card_spec_d02: "design_library/card_spec_d02.html",
card_spec_c01: "design_library/card_spec_c01.html",
card_spec_c02: "design_library/card_spec_c02.html",
card_spec_c03: "design_library/card_spec_c03.html",
card_spec_c04: "design_library/card_spec_c04.html",
card_spec_c05: "design_library/card_spec_c05.html",
card_spec_c06: "design_library/card_spec_c06.html",
card_spec_d03: "design_library/card_spec_d03.html",
card_spec_d04: "design_library/card_spec_d04.html",
card_spec_d05: "design_library/card_spec_d05.html",
card_spec_d06: "design_library/card_spec_d06.html",
design_spec: "design_library/design_spec.html",
}
impl View<'_> {
    pub fn body(&self) -> askama::Result<String> {
        self.render()
    }
    fn action_flash(&self) -> askama::Result<Html> {
        render(bilibi_ui::FlashGroup {
            id: "flash-group",
            flash: bilibi_ui::FlashMap {
                info: if self.state.submitted {
                    "The action link sent its request. Nothing changed."
                } else {
                    &self.state.notice
                },
                ..bilibi_ui::FlashMap::default()
            },
        })
    }
    /// The file input specimen, rendered from the uploads the state holds.
    fn file_input(&self) -> askama::Result<Html> {
        render(
            self.state
                .uploads
                .input(state::SAMPLE_UPLOAD, "design-library-file")
                .label("Files to keep")
                .hint(format!(
                    "Up to two files, {} KiB each.",
                    state::SAMPLE_UPLOAD_BYTES / 1024
                )),
        )
    }
    fn kept_size(&self, size: &usize) -> String {
        if *size < 1024 {
            format!("{size} B")
        } else {
            format!("{:.1} KiB", *size as f64 / 1024.0)
        }
    }
    fn dismissed(&self, id: &str) -> bool {
        self.state.dismissed.iter().any(|value| value == id)
    }
    fn title(&self) -> &str {
        match self.state.area.as_str() {
            "components" => "Components",
            "graphic" => "Graphic",
            "theme" => "Theme",
            _ => "Design Spec",
        }
    }
    fn description(&self) -> &str {
        match self.state.area.as_str() {
            "components" => {
                "Choose a component family, then review its current behaviour and states."
            }
            "graphic" => "Find an icon and copy its name.",
            "theme" => "The colours, type and spacing every page uses.",
            _ => "The design decisions every page follows.",
        }
    }
    fn stage(&self) -> askama::Result<Html> {
        let icon = render(bilibi_ui::Icon {
            name: "view",
            class: "size-3.5",
        })?;
        let label = match self.state.area.as_str() {
            "components" => "Current library",
            "design_spec" => "Accepted design",
            _ => "Development review",
        };
        Ok(Html::raw(format!(
            r#"<span id="design-library-stage" class="inline-flex items-center gap-1.5 rounded-full border border-line bg-surface px-2.5 py-1 text-xs font-medium text-ink-muted">{icon}{label}</span>"#
        )))
    }
    fn text(&self, expression: &str) -> String {
        let expression = expression.trim();
        if let Some(key) = expression
            .strip_prefix("@sample_form[:")
            .and_then(|s| s.strip_suffix("].value"))
        {
            return crate::controls::value_text(self.state.sample.get(key));
        }
        match expression {
            "@click_count" => self.state.clicks.to_string(),
            "elem(@confirm_outcome, 1)" => self.state.outcome.clone(),
            _ => String::new(),
        }
    }
    pub(crate) fn datetime(
        &self,
        id: &str,
        at: jiff::Timestamp,
        format: Format,
        precision: Precision,
    ) -> Html {
        let value = self.state.display.render(at, format, precision);
        Html::raw(format!(
            r#"<time id="{}" datetime="{}" data-hook="DateTime" data-instant="{}" data-text-company="{}" data-text-utc="{}" data-follow-shell="true" data-mode="{}" data-format="{}" data-precision="{}" class="tabular-nums">{}</time>"#,
            Html::text(id),
            Html::text(&value.iso),
            Html::text(&value.iso),
            Html::text(&value.company),
            Html::text(&value.utc),
            value.mode.as_str(),
            match format {
                Format::Date => "date",
                Format::Time => "time",
                _ => "datetime",
            },
            if precision == Precision::Second {
                "second"
            } else {
                "minute"
            },
            Html::text(&value.text)
        ))
    }
    fn table(&self, id: &str) -> askama::Result<Html> {
        if id == "design-library-confirm-rows" {
            let columns = &self.state.confirm_columns;
            let mut table = FlexTable::new(id, columns.flex_columns(), vec![]);
            table.event = "confirm-columns";
            table.zoom = columns.zoom;
            table.suggestions = columns.flex_suggestions();
            table.add_query = &columns.add_query;
            table.framed = false;
            table.actions = true;
            table.caption = "Example legal entity types";
            table.rows = self
                .state
                .confirm_rows
                .iter()
                .map(|key| {
                    let name = if *key == "sole" {
                        "Sole Proprietorship"
                    } else {
                        "Limited Liability Company"
                    };
                    let button_id = format!("design-library-confirm-delete-{key}");
                    let label = format!("Delete {name}");
                    let mut button = IconButton::new("delete", &label);
                    button.id = &button_id;
                    button.kind = IconKind::Danger;
                    button.event = "confirm-request";
                    button.name = "id";
                    button.value = key;
                    Ok(FlexRow {
                        key: key.to_string(),
                        dom_id: Some(format!("design-library-confirm-row-{key}")),
                        slots: [("name".into(), Html::text(name))].into(),
                        actions: render(button)?,
                        ..FlexRow::default()
                    })
                })
                .collect::<askama::Result<_>>()?;
            if table.rows.is_empty() {
                table.empty = Some(TableEmpty::State(EmptyState::missing(
                    "No example legal entity types",
                    "Every example has been deleted. Restore the examples to walk the flow again.",
                )));
            }
            return render(table);
        }
        if id == "sample-id-table" {
            let mut table = Table::new(id, vec![Column::new("Company ID").right()]);
            table.rows = (1..=5)
                .map(|id| Row {
                    id: format!("sample-id-{id}"),
                    cells: vec![Html::raw(format!(
                        "<span class=\"tabular-nums\">{id}</span>"
                    ))],
                    actions: Html::EMPTY,
                })
                .collect();
            return render(table);
        }
        if id == "design-library-empty-table" {
            let mut table = Table::new(id, vec![Column::new("Company"), Column::new("Status")]);
            table.caption = "Example empty table";
            let mut empty =
                EmptyState::missing("No suspended companies", "No company has this status.");
            empty.action = render(bilibi_ui::Button {
                body: Html::text("Show all statuses"),
                event: "test_click",
                ..bilibi_ui::Button::default()
            })?;
            table.empty = Some(TableEmpty::State(empty));
            return render(table);
        }
        let list = if id == "sample-table" {
            &self.state.list
        } else {
            &self.state.pattern
        };
        let columns = if id == "sample-table" {
            &self.state.sample_columns
        } else {
            &self.state.pattern_columns
        };
        let mut table = FlexTable::new(id, columns.flex_columns(), vec![]);
        table.event = if id == "sample-table" {
            "sample-columns"
        } else {
            "pattern-columns"
        };
        table.zoom = columns.zoom;
        table.suggestions = columns.flex_suggestions();
        table.add_query = &columns.add_query;
        table.framed = false;
        table.caption = "Example companies";
        if id == "sample-table" {
            for column in &mut table.columns {
                column.sort_id = Some(format!(
                    "sample-sort-{}",
                    if column.id == "updated_at" {
                        "updated"
                    } else {
                        &column.id
                    }
                ));
            }
            table.sort_event = "sample-sort";
            table.sort_by = list.sort_by.as_deref();
            table.sort_dir = list.sort_dir;
            table.actions = true;
        }
        let rows = state::companies(list);
        let counts = PageCounts::new(list.page, list.page_size, rows.len() as u64);
        table.rows = rows
            .into_iter()
            .skip(((counts.page - 1) * counts.page_size) as usize)
            .take(counts.page_size as usize)
            .map(|row| {
                let name = if id == "sample-table" {
                    Html::raw(format!(
                        "<span class=\"font-medium text-ink-strong\">{}</span>",
                        Html::text(&row.name)
                    ))
                } else {
                    Html::text(&row.name)
                };
                let status = match row.status {
                    "active" => BadgeKind::Success,
                    "pending" => BadgeKind::Warning,
                    _ => BadgeKind::Danger,
                };
                let dt_id = format!(
                    "{}-updated-{}",
                    if id == "sample-table" {
                        "sample"
                    } else {
                        "pattern"
                    },
                    row.id
                );
                let mut result = Row {
                    id: format!(
                        "{}-row-{}",
                        if id == "sample-table" {
                            "sample"
                        } else {
                            "pattern"
                        },
                        row.id
                    ),
                    cells: vec![
                        name,
                        Html::text(&row.code),
                        render(Badge::status(status, row.status))?,
                        self.datetime(&dt_id, row.updated, Format::Date, Precision::Minute),
                    ],
                    actions: Html::EMPTY,
                };
                if id == "sample-table" {
                    let label = format!("Preview {}", row.name);
                    let button_id = format!("sample-preview-{}", row.id);
                    let value = row.id.to_string();
                    let mut button = IconButton::new("view", &label);
                    button.id = &button_id;
                    button.event = "preview-company";
                    button.name = "id";
                    button.value = &value;
                    result.actions = render(button)?;
                }
                Ok(FlexRow {
                    key: row.id.to_string(),
                    dom_id: Some(result.id),
                    slots: ["name", "code", "status", "updated_at"]
                        .into_iter()
                        .map(str::to_owned)
                        .zip(result.cells)
                        .collect(),
                    actions: result.actions,
                    ..FlexRow::default()
                })
            })
            .collect::<askama::Result<_>>()?;
        let mut empty = EmptyState::missing(
            format!("No companies match “{}”", list.search),
            "Check the spelling, or clear the search to see every example company.",
        );
        empty.action = render(bilibi_ui::Button {
            id: "sample-clear-search",
            event: "sample-clear-search",
            body: Html::text("Clear search"),
            ..bilibi_ui::Button::default()
        })?;
        if table.rows.is_empty() {
            table.empty = Some(if id == "sample-table" {
                TableEmpty::State(empty)
            } else {
                TableEmpty::Content(Html::raw(
                    r#"<p id="design-library-pattern-empty" class="p-4 text-sm text-ink-muted">No example companies match. Clear the search to see all examples.</p>"#,
                ))
            });
        }
        render(table)
    }
    fn pagination(&self, id: &str) -> askama::Result<Html> {
        let sample = id == "component-pagination";
        let list = if sample {
            &self.state.list
        } else {
            &self.state.pattern
        };
        let count = state::companies(list).len() as u64;
        let mut p = Pagination::new(
            id,
            PageCounts::new(list.page, list.page_size, count),
            &[25, 50, 100, 300],
        );
        p.page_event = if sample {
            "sample-page"
        } else {
            "pattern-page"
        };
        p.filters_event = if sample {
            "sample-page-size"
        } else {
            "pattern-page-size"
        };
        render(p)
    }
    fn flex(&self, id: &str) -> askama::Result<Html> {
        let instant = |seconds| {
            let at = jiff::Timestamp::from_second(seconds).expect("sample timestamp");
            let value = self
                .state
                .display
                .render(at, Format::DateTime, Precision::Minute);
            bilibi_ui::Instant::new(
                &value.iso,
                &value.text,
                &value.company,
                &value.utc,
                value.mode.as_str(),
            )
        };
        if id == "sample-flex-table" {
            render(self.state.flex.framed(id, "sample-flex", &instant))
        } else {
            render(self.state.plain.plain(id, "sample-flex-plain", &instant))
        }
    }
    fn confirm_dialog(&self) -> askama::Result<Html> {
        let Some(key) = &self.state.confirm_request else {
            return Ok(Html::EMPTY);
        };
        let name = if key == "sole" {
            "Sole Proprietorship"
        } else {
            "Limited Liability Company"
        };
        render(bilibi_ui::ConfirmDialog {
            id: "design-library-confirm",
            consequence: &format!("Legal entity type “{name}” will be deleted."),
            detail: "It can no longer be chosen for a company. This cannot be undone.",
            confirm: "Delete",
            working: "Deleting…",
            confirm_event: "confirm-run",
            cancel_event: "confirm-cancel",
        })
    }
    fn navigation(&self) -> askama::Result<Html> {
        let leaf = |id: &str, label: &str, icon: &str, active| bilibi_ui::NavNode {
            id: id.into(),
            label: label.into(),
            icon: icon.into(),
            active,
            leading: false,
            route: Some(format!("{}/components", crate::BASE)),
            children: Vec::new(),
        };
        let rows = bilibi_ui::navigation_unpinned(&[
            bilibi_ui::NavNode {
                leading: true,
                ..leaf("example.dashboard", "Dashboard", "hero-squares-2x2", false)
            },
            leaf(
                "example.companies",
                "Companies",
                "hero-building-office-2",
                false,
            ),
            bilibi_ui::NavNode {
                id: "example.system".into(),
                label: "System".into(),
                icon: "hero-cog-6-tooth".into(),
                route: None,
                active: false,
                leading: false,
                children: vec![leaf(
                    "example.system.design-library",
                    "Design Library",
                    "hero-paint-brush",
                    true,
                )],
            },
        ])?;
        Ok(Html::raw(format!(
            r#"<nav id="example-nav" aria-label="Example menu" class="app-nav-menu max-w-xs rounded-lg bg-surface-sidebar px-0.5 py-0.5">{rows}</nav>"#
        )))
    }
    fn split_handle(&self, id: &str) -> askama::Result<Html> {
        let (h, label, class) = match id {
            "design-library-split-handle-h" => {
                (true, "Resize Companies and Users", "left-1/2 top-0 h-full")
            }
            "design-library-split-handle-v" => (
                false,
                "Resize Users and Employees",
                "left-1/2 top-1/2 w-1/2",
            ),
            _ => (true, "A handle the caller has not placed", ""),
        };
        #[derive(Template)]
        #[template(
            source = r#"{% import "tiling.html" as tile %}{% call tile::split_handle(id=id,horizontal=horizontal,label=label,place="",split="",rect="",class=class) %}{% endcall %}"#,
            ext = "html"
        )]
        struct Handle<'a> {
            id: &'a str,
            horizontal: bool,
            label: &'a str,
            class: &'a str,
        }
        render(Handle {
            id,
            horizontal: h,
            label,
            class,
        })
    }

    fn empty(&self, id: &str) -> askama::Result<Html> {
        let mut e = match id {
            "design-library-empty-forbidden" => EmptyState::forbidden("view companies"),
            "design-library-empty-filtered" => EmptyState::missing(
                "No companies match “acme”",
                "Check the spelling, or clear the search.",
            ),
            _ => EmptyState::missing("No companies yet", "Companies you add appear here."),
        }
        .id(id);
        if id != "design-library-empty-forbidden" {
            e.action = render(bilibi_ui::Button {
                body: Html::text(if id == "design-library-empty-filtered" {
                    "Clear search"
                } else {
                    "Add Company"
                }),
                event: "test_click",
                variant: if id == "design-library-empty-filtered" {
                    bilibi_ui::ButtonVariant::Secondary
                } else {
                    bilibi_ui::ButtonVariant::Primary
                },
                ..bilibi_ui::Button::default()
            })?;
        }
        render(e)
    }
}

pub fn icon_names() -> impl Iterator<Item = &'static str> {
    bilibi_ui::icons::GLYPHS
        .iter()
        .map(|g| g.name)
        .chain(bilibi_ui::icons::ACTIONS.iter().map(|(name, _)| *name))
}
impl View<'_> {
    fn catalog(&self) -> askama::Result<Html> {
        let filter = self.state.icon_search.trim().to_lowercase();
        let total = icon_names().count();
        let shown = icon_names().filter(|name| name.contains(&filter)).count();
        let mut toolbar = bilibi_ui::FilterToolbar::new(
            "icon-catalog-filters",
            vec![bilibi_ui::ToolbarControl::search(
                "icon-catalog-search",
                "search",
                "Filter icons",
                &self.state.icon_search,
                "Filter by name…",
            )],
        );
        toolbar.event = "icon-filter";
        let mut html = Html::raw(
            r#"<div><h2 class="text-base font-semibold text-ink-strong">Icon catalogue</h2><p class="text-sm text-ink-muted">Every name the icon registry resolves. Select a tile to copy the name a screen passes to the icon component. Any other Heroicon is reached by its own <code>hero-</code> name.</p></div><div class="space-y-3 rounded-xl border border-line bg-surface p-4 shadow-xs">"#,
        );
        html.push(&render(toolbar)?);
        html.push(&Html::raw(format!(r#"<div class="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 text-xs"><p id="icon-catalog-count" class="text-ink-muted">{}</p><p id="icon-catalog-status" role="status" class="text-success-ink">{}</p></div>"#,
            if filter.is_empty(){format!("{total} icons")}else{format!("{shown} of {total} icons")},
            self.state.icon_copy.as_ref().filter(|(_,copied)|*copied).map(|(name,_)|Html::text(&format!("Copied “{name}” to the clipboard.")).to_string()).unwrap_or_default())));
        if let Some((name, false)) = &self.state.icon_copy {
            html.push(&Html::raw(format!(r#"<p id="icon-catalog-copy-failed" role="alert" class="rounded-lg border border-danger-line bg-danger-surface px-3 py-2 text-sm text-danger-ink">The browser did not allow copying “{}”. Select the name on the tile and copy it by hand.</p>"#,Html::text(name))));
        }
        html.push(&Html::raw(r#"<div id="icon-catalog" data-hook="ClipboardCopy" data-copy-event="icon-copied" class="space-y-5">"#));
        for (id, label, names) in [
            (
                "glyphs",
                "Custom glyphs",
                bilibi_ui::icons::GLYPHS
                    .iter()
                    .map(|g| g.name)
                    .collect::<Vec<_>>(),
            ),
            (
                "actions",
                "Actions",
                bilibi_ui::icons::ACTIONS
                    .iter()
                    .map(|(name, _)| *name)
                    .collect(),
            ),
        ] {
            let mut names: Vec<_> = names
                .into_iter()
                .filter(|name| name.contains(&filter))
                .collect();
            names.sort_unstable();
            if names.is_empty() {
                continue;
            }
            html.push(&Html::raw(format!(r#"<div id="icon-catalog-{id}"><div class="mb-2 flex items-baseline justify-between gap-3"><h3 class="text-sm font-semibold text-ink">{label}</h3><span class="text-xs text-ink-muted">{}</span></div><div class="grid grid-cols-2 gap-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-6">"#,names.len())));
            for name in names {
                let glyph = render(bilibi_ui::Icon {
                    name,
                    class: "size-5 text-ink group-hover:text-action",
                })?;
                let copied = self
                    .state
                    .icon_copy
                    .as_ref()
                    .is_some_and(|(n, c)| n == name && *c);
                let hero=bilibi_ui::icons::action(name).map(|hero|format!(r#"<span class="w-full truncate text-[0.6875rem] text-ink-muted">{hero}</span>"#)).unwrap_or_default();
                let status = if copied {
                    format!(
                        r#"<span id="icon-tile-{name}-copied" class="absolute right-1.5 top-1 text-[0.6875rem] font-medium text-success-ink">Copied</span>"#
                    )
                } else {
                    String::new()
                };
                html.push(&Html::raw(format!(r#"<button type="button" id="icon-tile-{name}" data-copy-text="{name}" title="Copy “{name}”" class="group relative flex min-w-0 flex-col items-center gap-1 rounded-lg border bg-canvas px-2 pb-2 pt-3 text-center hover:bg-surface-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-strong/40 {}">{glyph}<span class="w-full truncate font-mono text-xs text-ink"><span class="sr-only">Copy </span>{name}</span>{hero}{status}</button>"#,if copied{"border-success-line"}else{"border-line hover:border-high-contrast-line"})));
            }
            html.push(&Html::raw("</div></div>"));
        }
        if shown == 0 {
            let mut empty = EmptyState::missing(
                format!("No icons match “{}”", self.state.icon_search.trim()),
                "Clear the filter to see every icon.",
            )
            .id("icon-catalog-empty");
            empty.class = "py-6".into();
            empty.action = render(bilibi_ui::Button {
                id: "icon-catalog-clear",
                event: "icon-clear-filter",
                body: Html::text("Clear filter"),
                ..bilibi_ui::Button::default()
            })?;
            html.push(&render(empty)?);
        }
        html.push(&Html::raw("</div></div>"));
        Ok(html)
    }
}
