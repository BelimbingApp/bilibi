//! The component specimen: every shared primitive and variant on one page,
//! served unauthenticated at `/assets/ui-components.html`. It is the screenshot
//! baseline for parity with Bilimbi's Design Library specimens, not the Design
//! Library itself. Secondary slots are built through the typed models, so the
//! page exercises both the macro and the Rust paths.
use crate::inline_specimen::InlineSamples;
use crate::section::sections;
use crate::{
    choices, error_sentence, ActionLink, AddMore, BadgeKind, Button, ButtonVariant, Card, Choice,
    Chrome, ClientEntry, CodeInput, Combobox, ErrorSummary, Field, FileInput, Html, IconButton,
    IconContext, IconKind, InputKind, LocationAction, MultiSelect, PageVariant, RadioGroup,
    Restricted, RestrictedField, SecretInput, StatItem, UploadSpec, Uploads,
};
use askama::Template;

/// The page is cut into sections, each its own template; see `section`.
#[derive(Template)]
#[template(path = "ui_components_specimen.html")]
pub struct ComponentsSpecimen {
    header_actions: Html,
    header_title_actions: Html,
    section_title_actions: Html,
    section_actions: Html,
    card_titled: Html,
    card_untitled: Html,
    card_flat: Html,
    stats_linked: Vec<StatItem>,
    stats_unlinked: Vec<StatItem>,
    stats_three: Vec<StatItem>,
    controls: FormControls,
    colours: Vec<crate::ColourInput>,
    /// The help, error and locked fields, rendered through the typed models.
    field_states: Html,
    radio_disabled: Html,
    inline: InlineSamples,
    flex: crate::flex_sample::FlexSpecimens,
    /// Every chart kind, then the empty and refused states.
    charts: Vec<Html>,
    /// The file inputs in every state the host can leave them in: empty,
    /// arriving, complete, refused per entry and refused as a whole.
    uploads: Uploads,
    /// The browser location action in both button variants and disabled.
    location_secondary: LocationAction,
    location_primary: LocationAction,
    location_disabled: LocationAction,
}

sections! {
    ComponentsSpecimen {
        layout: "ui_components_specimen/layout.html",
        records: "ui_components_specimen/records.html",
        buttons: "ui_components_specimen/buttons.html",
        links: "ui_components_specimen/links.html",
        code_input: "ui_components_specimen/code_input.html",
        text_inputs: "ui_components_specimen/text_inputs.html",
        text_inputs_basic: "ui_components_specimen/text_inputs_basic.html",
        text_inputs_typed: "ui_components_specimen/text_inputs_typed.html",
        choices: "ui_components_specimen/choices.html",
        colour_inputs: "ui_components_specimen/colour_inputs.html",
        multi_select: "ui_components_specimen/multi_select.html",
        date_secret: "ui_components_specimen/date_secret.html",
        date_inputs: "ui_components_specimen/date_inputs.html",
        secret_inputs: "ui_components_specimen/secret_inputs.html",
        stored_secret: "ui_components_specimen/stored_secret.html",
        input_variants: "ui_components_specimen/input_variants.html",
        combobox: "ui_components_specimen/combobox.html",
        file_input: "ui_components_specimen/file_input.html",
        location_action: "ui_components_specimen/location_action.html",
        restricted: "ui_components_specimen/restricted.html",
        form_helpers: "ui_components_specimen/form_helpers.html",
        inline_facts: "ui_components_specimen/inline_facts.html",
        flex_tables: "ui_components_specimen/flex_tables.html",
        charts: "ui_components_specimen/charts.html",
    }
}

/// The form controls of the specimen, mirroring Bilimbi's Design Library
/// sample form (`sample[...]` fields) so one reference renders both.
pub struct FormControls {
    pub proof: crate::ProofInput,
    pub code: CodeInput,
    pub authenticator: CodeInput,
    pub optional_authenticator: CodeInput,
    pub invalid_authenticator: CodeInput,
    pub text: Field,
    pub search: Field,
    pub email: Field,
    pub tel: Field,
    pub number: Field,
    pub url: Field,
    pub textarea: Field,
    pub select: Field,
    pub checkbox: Field,
    pub radio: RadioGroup,
    pub required_roles: MultiSelect,
    pub chip_roles: MultiSelect,
    pub plain_roles: MultiSelect,
    pub locked_region: RadioGroup,
    pub date: Field,
    pub time: Field,
    pub datetime: Field,
    pub month: Field,
    pub week: Field,
    pub password: SecretInput,
    pub api_key: SecretInput,
    pub stored_key: SecretInput,
    pub no_eye: SecretInput,
    pub example_role: Field,
    pub example_teams: Field,
    pub example_interests: MultiSelect,
    pub example_teams_chips: MultiSelect,
    pub quick_search: Field,
    pub combobox_country: Combobox,
    pub combobox_selected: Combobox,
    pub combobox_empty: Combobox,
    pub combobox_inline: Combobox,
    pub combobox_account: Combobox,
    pub combobox_account_inline: Combobox,
    pub restricted: Restricted,
    pub restricted_reason: Restricted,
    pub restricted_field: RestrictedField,
    pub restricted_field_reason: RestrictedField,
    /// The form helpers around the fields: the problems listed after a
    /// refused save, a folded rarely-used part, and "+ Add …" for one more
    /// optional field.
    pub summary: ErrorSummary,
    pub add_more: AddMore,
    pub folded: Field,
}

const ROLES: [(&str, &str); 5] = [
    ("Administrator", "admin"),
    ("Operator", "operator"),
    ("Reviewer", "reviewer"),
    ("Auditor", "auditor"),
    ("Support coordinator", "support"),
];
const TEAMS: [(&str, &str); 3] = [
    ("Platform", "platform"),
    ("Billing", "billing"),
    ("Support", "support"),
];
/// Records with a code after the name, as a ledger's accounts are.
fn accounts() -> Vec<Choice> {
    [
        ("Cash at bank", "1000"),
        ("Trade receivables", "1100"),
        ("Inventory", "1200"),
        ("Prepaid expenses", "1300"),
        ("Property and equipment", "1500"),
        ("Trade payables", "2000"),
        ("Accrued expenses", "2100"),
        ("Retained earnings", "3100"),
        ("Product sales", "4000"),
        ("Cost of goods sold", "5000"),
        ("Rent", "6100"),
        ("Salaries and wages", "6200"),
    ]
    .into_iter()
    .map(|(name, code)| Choice::new(name, code).detail(code))
    .collect()
}

const COUNTRIES: [(&str, &str); 3] = [
    ("Malaysia (MY)", "MY"),
    ("Singapore (SG)", "SG"),
    ("Thailand (TH)", "TH"),
];
const APPEARANCE: [(&str, &str); 3] = [("System", "system"), ("Light", "light"), ("Dark", "dark")];

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

impl FormControls {
    fn new() -> Self {
        let sample = |key: &str, value: &str| Field::in_form("sample", key).value(value);
        Self {
            proof: crate::ProofInput::new(
                "specimen-proof",
                "specimen[proof]",
                vec![
                    crate::ProofMethod::new("example.app", "Authenticator app", 6, true),
                    crate::ProofMethod::new("example.device", "Security device", 8, false),
                ],
            ),
            code: CodeInput::new("specimen_code", "specimen[code]", "Device code", 8)
                .value("ABCD-EF23")
                .group_size(4),
            authenticator: CodeInput::new("specimen_otp", "specimen[otp]", "Authenticator code", 6)
                .numeric(),
            optional_authenticator: CodeInput::new(
                "specimen_optional_otp",
                "specimen[optional_otp]",
                "2FA code (role changes only)",
                6,
            )
            .numeric()
            .optional(),
            invalid_authenticator: CodeInput::new(
                "specimen_otp_error",
                "specimen[otp_error]",
                "Authenticator code",
                6,
            )
            .numeric()
            .errors(vec!["That code was not accepted. Enter a new code.".into()]),
            text: sample("text_field", "Sample text value")
                .label("Text")
                .placeholder("Enter a value"),
            search: sample("search_field", "Search query")
                .kind(InputKind::Search)
                .label("Search")
                .placeholder("Search…"),
            email: sample("email_field", "hello@example.com")
                .kind(InputKind::Email)
                .label("Email"),
            tel: sample("tel_field", "+60 12-345 6789")
                .kind(InputKind::Tel)
                .label("Telephone"),
            number: sample("number_field", "25")
                .kind(InputKind::Number)
                .label("Number"),
            url: sample("url_field", "https://bilimbi.test")
                .kind(InputKind::Url)
                .label("URL"),
            textarea: sample(
                "textarea_field",
                "Multi-line sample content demonstrating textarea rendering.",
            )
            .kind(InputKind::Textarea)
            .label("Textarea")
            .wrapper_class("mb-0"),
            select: sample("select_field", "standard")
                .kind(InputKind::Select)
                .label("Select")
                .options(choices(&[
                    ("Standard", "standard"),
                    ("Advanced", "advanced"),
                ])),
            checkbox: Field::in_form("sample", "checkbox_field")
                .kind(InputKind::Checkbox)
                .label("Enable synchronization")
                .checked(true),
            radio: RadioGroup::in_form("sample", "radio_field", choices(&APPEARANCE))
                .id("specimen-radio-group")
                .label("Appearance")
                .value("system")
                .wrapper_class("mb-0"),
            required_roles: MultiSelect::in_form("sample", "required_roles")
                .id("specimen-multi-select-required")
                .label("Roles")
                .hint("Everyone needs at least one role.")
                .required()
                .options(choices(&ROLES))
                .values(strings(&[
                    "admin", "operator", "reviewer", "auditor", "support",
                ]))
                .selection_label(":count role selected|:count roles selected"),
            chip_roles: MultiSelect::in_form("sample", "chip_roles")
                .id("specimen-multi-select-chips")
                .label("Wrapping roles")
                .wrap_chips()
                .chips("sample_chip_remove")
                .options(choices(&ROLES))
                .values(strings(&[
                    "admin", "operator", "reviewer", "auditor", "support",
                ])),
            plain_roles: MultiSelect::in_form("sample", "plain_roles")
                .id("specimen-multi-select-plain")
                .aria_label("Optional roles")
                .options(choices(&ROLES))
                .wrapper_class("mb-0"),
            locked_region: RadioGroup::new(
                "specimen-radio-locked",
                "locked_region",
                choices(&[("Singapore", "sg"), ("Malaysia", "my")]),
            )
            .aria_label("Data region")
            .value("sg")
            .disabled()
            .hint("Your organization set this for everyone.")
            .wrapper_class("mb-0"),
            date: sample("date_field", "2026-08-17")
                .kind(InputKind::Date)
                .label("Date"),
            time: sample("time_field", "14:30")
                .kind(InputKind::Time)
                .label("Time"),
            datetime: sample("datetime_field", "2026-08-17T14:30")
                .kind(InputKind::DatetimeLocal)
                .label("Date and time"),
            month: sample("month_field", "2026-08")
                .kind(InputKind::Month)
                .label("Month"),
            week: sample("week_field", "2026-W34")
                .kind(InputKind::Week)
                .label("Week"),
            password: SecretInput::in_form("sample", "password_field", "password")
                .value("secret-value")
                .label("Password")
                .autocomplete("new-password"),
            api_key: SecretInput::in_form("sample", "api_key_field", "API key")
                .value("sk-sample-0000")
                .label("API key")
                .autocomplete("off")
                .wrapper_class("mb-0")
                .hint("Use the eye to check what you typed."),
            stored_key: SecretInput::new("specimen-stored-key", "stored_key", "API key")
                .label("Stored API key")
                .stored()
                .stored_reveal("reveal-stored", "stored_key")
                .value("never-render-this-plaintext")
                .autocomplete("off")
                .hint("Leave the dots to keep the saved value, or press Clear to remove it."),
            no_eye: SecretInput::new("specimen-no-eye", "sample[password_field]", "password")
                .value("secret-value")
                .aria_label("Password without reveal")
                .no_reveal(),
            example_role: Field::named("example_role")
                .kind(InputKind::Select)
                .label("Role")
                .prompt("Select a role")
                .options(choices(&[
                    ("Administrator", "admin"),
                    ("Operator", "operator"),
                ])),
            example_teams: Field::named("example_teams[]")
                .id("example_teams")
                .kind(InputKind::Select)
                .label("Teams")
                .multiple()
                .options(choices(&TEAMS)),
            example_interests: MultiSelect::in_form("sample", "example_interests")
                .id("example_interests")
                .label("Interests")
                .selection_label(":count interest selected|:count interests selected")
                .options(choices(&[("Platform", "platform"), ("Billing", "billing")]))
                .values(strings(&["platform"])),
            example_teams_chips: MultiSelect::in_form("sample", "example_teams_chips")
                .id("example_teams_chips")
                .label("Teams, as chips")
                .placeholder("Add teams")
                .chips("sample_chip_remove")
                .options(choices(&TEAMS))
                .values(strings(&["platform"])),
            quick_search: Field::named("quick_search")
                .kind(InputKind::Search)
                .aria_label("Quick search")
                .placeholder("Quick search…"),
            combobox_country: Combobox::new("specimen-combobox-country", "example_country")
                .label("Country")
                .placeholder("Search countries…")
                .options(choices(&COUNTRIES)),
            combobox_selected: Combobox::new(
                "specimen-combobox-selected",
                "example_selected_country",
            )
            .label("Selected country")
            .value("SG")
            .options(choices(&COUNTRIES)),
            combobox_empty: Combobox::new("specimen-combobox-empty", "example_empty_country")
                .label("Unavailable country list")
                .placeholder("No countries")
                .errors(strings(&["Choose a country before continuing."])),
            combobox_inline: Combobox::new("specimen-combobox-inline", "")
                .aria_label("Inline country editor")
                .value("MY")
                .cancel_event("preview-combobox-cancel")
                .options(choices(&COUNTRIES)),
            combobox_account: Combobox::new("specimen-combobox-account", "example_account")
                .label("Account")
                .placeholder("Search accounts…")
                .value("2000")
                .options(accounts()),
            combobox_account_inline: Combobox::new("specimen-combobox-account-inline", "")
                .aria_label("Inline account editor")
                .value("1100")
                .cancel_event("preview-combobox-cancel")
                .options(accounts()),
            restricted: Restricted::new("specimen-restricted"),
            restricted_reason: Restricted::new("specimen-restricted-reason")
                .reason("Bonuses are shown to payroll only."),
            restricted_field: RestrictedField::new("specimen-restricted-field", "Bank account"),
            restricted_field_reason: RestrictedField::new(
                "specimen-restricted-field-reason",
                "Bonus",
            )
            .reason("Bonuses are shown to payroll only."),
            summary: {
                let mut summary = ErrorSummary::new("specimen-error-summary");
                summary.push(
                    "sample_text_field",
                    error_sentence("Text", "can't be blank"),
                );
                summary.push(
                    "sample_number_field",
                    error_sentence("Number", "must be at most 100"),
                );
                summary
            },
            add_more: AddMore::new("specimen-add-more", "Add address line 2", "add_line"),
            folded: Field::in_form("sample", "source")
                .id("specimen-folded-source")
                .label("Source")
                .value("manual"),
        }
    }
}

impl ComponentsSpecimen {
    pub fn new() -> askama::Result<Self> {
        let header_actions = Html::render(&Button {
            variant: ButtonVariant::Primary,
            body: Html::text("Save changes"),
            ..Button::default()
        })?;
        let header_title_actions = Html::render(&IconButton {
            context: IconContext::Inline,
            ..IconButton::new("edit", "Edit company")
        })?;
        let section_title_actions = Html::render(&IconButton {
            context: IconContext::Inline,
            ..IconButton::new("edit", "Edit location")
        })?;
        let section_actions = Html::render(&ActionLink {
            id: "specimen-section-heading-manage",
            icon: "manage",
            href: "/system/design-library",
            title: "Manage this section's records",
            body: Html::text("Manage"),
            method: "",
        })?;
        let card_titled = Html::render(&Card {
            title: "Company profile",
            inner_class: "p-3",
            body: Html::raw(
                r#"<p class="text-sm text-ink">A title adds the bordered heading strip.</p>"#,
            ),
            ..Card::default()
        })?;
        let card_untitled = Html::render(&Card {
            inner_class: "p-3",
            body: Html::raw(
                r#"<p class="text-sm text-ink">Without one, the body stands alone.</p>"#,
            ),
            ..Card::default()
        })?;
        let card_flat = Html::render(&Card {
            inner_class: "p-0",
            body: Html::raw(
                r#"<p class="px-3 py-2 text-sm text-ink">p-0 drops the radius so a framed table picks up no corner.</p>"#,
            ),
            ..Card::default()
        })?;
        let field_states = [
            Field::named("field-with-help")
                .label("Company code")
                .value("acme")
                .hint("Used in URLs and exports."),
            Field::in_form("error_sample", "invalid_text")
                .label("Required value")
                .placeholder("Enter a value")
                .error("can't be blank"),
            Field::named("disabled-field")
                .label("System value")
                .value("Managed by Bilimbi")
                .disabled(),
            Field::named("required-field")
                .label("Company name")
                .placeholder("Enter a value")
                .required(),
            Field::named("readonly-field")
                .label("System identifier")
                .value("acme-0042")
                .readonly(),
            Field::named("field-with-help-and-error")
                .label("Tax ID")
                .hint("As printed on the certificate.")
                .error("can't be blank"),
        ]
        .iter()
        .map(Html::render)
        .collect::<askama::Result<Html>>()?;
        let radio_disabled = Html::render(
            &RadioGroup::new(
                "specimen-radio-disabled",
                "radio_disabled",
                choices(&APPEARANCE),
            )
            .label("Locked appearance")
            .value("system")
            .disabled()
            .wrapper_class("mb-0 mt-4"),
        )?;
        Ok(Self {
            header_actions,
            card_titled,
            card_untitled,
            card_flat,
            header_title_actions,
            section_title_actions,
            section_actions,
            stats_linked: vec![
                StatItem::number("People", 24),
                StatItem::number("Active", 18),
            ],
            stats_unlinked: vec![StatItem::text("Store", "Durable")],
            stats_three: vec![
                StatItem::number("Total", 62),
                StatItem::number("Active", 58),
                StatItem::text("Current", "MY"),
            ],
            controls: FormControls::new(),
            colours: colour_inputs(),
            field_states,
            radio_disabled,
            inline: InlineSamples::new()?,
            flex: crate::flex_sample::FlexSpecimens::new()?,
            charts: specimen_charts()?,
            uploads: specimen_uploads(),
            location_secondary: LocationAction::new(
                "specimen-location",
                "locate",
                "Use my location",
            ),
            location_primary: LocationAction::new("specimen-location-clock", "clock", "Clock in")
                .variant(ButtonVariant::Primary)
                .value("clock_type", "in")
                .sending("Recording clock event…"),
            location_disabled: LocationAction::new(
                "specimen-location-disabled",
                "clock",
                "Clock out",
            )
            .value("clock_type", "out")
            .disabled(),
        })
    }

    fn file_empty(&self) -> FileInput<'_> {
        self.uploads
            .input("statement", "specimen-file-empty")
            .label("Bank statement")
            .hint("A CSV export, at most 5 MiB.")
    }
    fn file_arriving(&self) -> FileInput<'_> {
        self.uploads
            .input("documents", "specimen-file-arriving")
            .label("Source documents")
    }
    fn file_refused(&self) -> FileInput<'_> {
        self.uploads
            .input("scan", "specimen-file-refused")
            .label("Signed scan")
    }
    fn file_disabled(&self) -> FileInput<'_> {
        self.uploads
            .input("locked", "specimen-file-disabled")
            .label("Locked record")
            .hint("A final record accepts no more files.")
            .disabled()
    }
}

/// The states the specimen shows, produced the way the host produces them:
/// through registration and chunk appends, never by hand.
/// Sample charts: one of each kind over neutral example data, a series with a
/// gap and negative values, then a chart with no data and a pie refused for
/// having too many slices (its data still shown).
fn specimen_charts() -> askama::Result<Vec<Html>> {
    use crate::{Chart, ChartKind, Series};
    let months: Vec<String> = ["Jan", "Feb", "Mar", "Apr", "May", "Jun"]
        .map(String::from)
        .to_vec();
    let series = |name: &str, values: &[Option<f64>]| Series {
        name: name.into(),
        values: values.to_vec(),
    };
    let regions: Vec<String> = ["North", "South", "East", "West"]
        .map(String::from)
        .to_vec();
    let shares = series(
        "Orders",
        &[Some(420.0), Some(310.0), Some(180.0), Some(95.0)],
    );
    let many: Vec<String> = (1..=10).map(|i| format!("Region {i}")).collect();
    let charts = [
        Chart::new(
            "sample-chart-kpi",
            "Orders this month",
            ChartKind::Kpi,
            vec![],
            vec![series("Orders this month", &[Some(1284.0)])],
        ),
        Chart::new(
            "sample-chart-bar",
            "Orders by month",
            ChartKind::Bar,
            months.clone(),
            vec![series(
                "Orders",
                &[
                    Some(120.0),
                    Some(160.0),
                    Some(140.0),
                    Some(210.0),
                    Some(190.0),
                    Some(240.0),
                ],
            )],
        ),
        Chart::new(
            "sample-chart-grouped",
            "Net stock change by month",
            ChartKind::Bar,
            months.clone(),
            vec![
                series(
                    "North",
                    &[
                        Some(40.0),
                        Some(-15.0),
                        Some(25.0),
                        Some(-30.0),
                        Some(10.0),
                        Some(35.0),
                    ],
                ),
                series(
                    "South",
                    &[
                        Some(20.0),
                        Some(30.0),
                        Some(-10.0),
                        Some(15.0),
                        None,
                        Some(-5.0),
                    ],
                ),
            ],
        ),
        Chart::new(
            "sample-chart-stacked",
            "Cases by priority",
            ChartKind::StackedBar,
            months.clone(),
            vec![
                series(
                    "Low",
                    &[
                        Some(30.0),
                        Some(28.0),
                        Some(35.0),
                        Some(32.0),
                        Some(40.0),
                        Some(38.0),
                    ],
                ),
                series(
                    "Normal",
                    &[
                        Some(52.0),
                        Some(60.0),
                        Some(48.0),
                        Some(66.0),
                        Some(58.0),
                        Some(70.0),
                    ],
                ),
                series(
                    "High",
                    &[
                        Some(12.0),
                        Some(9.0),
                        Some(15.0),
                        Some(11.0),
                        Some(18.0),
                        Some(14.0),
                    ],
                ),
            ],
        ),
        Chart::new(
            "sample-chart-line",
            "Average resolution hours",
            ChartKind::Line,
            months.clone(),
            vec![
                series(
                    "Email",
                    &[
                        Some(18.5),
                        Some(16.2),
                        None,
                        Some(14.8),
                        Some(13.1),
                        Some(12.4),
                    ],
                ),
                series(
                    "Chat",
                    &[
                        Some(4.2),
                        Some(3.9),
                        Some(4.5),
                        Some(3.6),
                        Some(3.2),
                        Some(3.4),
                    ],
                ),
            ],
        ),
        Chart::new(
            "sample-chart-pie",
            "Orders by region",
            ChartKind::Pie,
            regions.clone(),
            vec![shares.clone()],
        ),
        Chart::new(
            "sample-chart-donut",
            "Orders by region (total)",
            ChartKind::Donut,
            regions,
            vec![shares],
        ),
        Chart::new_with_display_values(
            "sample-chart-decimal-kpi",
            "Stored quantity",
            ChartKind::Kpi,
            vec![],
            vec![series("Quantity", &[Some(47.6)])],
            vec![vec![Some("47.60".into())]],
        ),
        Chart::new_with_display_values(
            "sample-chart-decimal-bar",
            "Stored quantities",
            ChartKind::Bar,
            vec!["North".into(), "South".into()],
            vec![series("Quantity", &[Some(47.6), Some(47.6)])],
            vec![vec![Some("47.60".into()), Some("47.6".into())]],
        ),
        Chart::new(
            "sample-chart-small-donut",
            "Small quantities",
            ChartKind::Donut,
            vec!["North".into(), "South".into()],
            vec![series("Quantity", &[Some(0.0001), Some(0.0002)])],
        ),
        Chart::new(
            "sample-chart-empty",
            "Orders by month (no orders yet)",
            ChartKind::Bar,
            months.clone(),
            vec![series("Orders", &[None, None, None, None, None, None])],
        ),
        Chart::new(
            "sample-chart-refused",
            "Orders by region (ten regions)",
            ChartKind::Pie,
            many,
            vec![series(
                "Orders",
                &[
                    Some(5.0),
                    Some(8.0),
                    Some(3.0),
                    Some(9.0),
                    Some(4.0),
                    Some(7.0),
                    Some(6.0),
                    Some(2.0),
                    Some(1.0),
                    Some(10.0),
                ],
            )],
        ),
        // Twelve longer names turn to fit; small values keep their own scale;
        // trillion-scale ticks read in a few characters.
        Chart::new(
            "sample-chart-turned",
            "Headcount by department",
            ChartKind::Bar,
            [
                "Customer Support",
                "Engineering",
                "Finance",
                "Human Resources",
                "Legal",
                "Marketing",
                "Operations",
                "Procurement",
                "Product Management",
                "Quality Assurance",
                "Research",
                "Sales",
            ]
            .map(String::from)
            .to_vec(),
            vec![series(
                "People",
                &[
                    Some(42.0),
                    Some(118.0),
                    Some(17.0),
                    Some(9.0),
                    Some(6.0),
                    Some(23.0),
                    Some(35.0),
                    Some(8.0),
                    Some(14.0),
                    Some(19.0),
                    Some(11.0),
                    Some(51.0),
                ],
            )],
        ),
        Chart::new(
            "sample-chart-small",
            "Defect rate by month",
            ChartKind::Line,
            months.clone(),
            vec![series(
                "Rate",
                &[
                    Some(0.0001),
                    Some(0.00012),
                    Some(0.00015),
                    Some(0.00013),
                    Some(0.00018),
                    Some(0.0002),
                ],
            )],
        ),
        Chart::new(
            "sample-chart-large",
            "Turnover by region",
            ChartKind::Bar,
            ["North", "South", "East", "West"]
                .map(String::from)
                .to_vec(),
            vec![series(
                "Turnover",
                &[
                    Some(1.25e12),
                    Some(3.4e12),
                    Some(2.75e12),
                    Some(987_654_321_012.5),
                ],
            )],
        ),
    ];
    charts.iter().map(Html::render).collect()
}

fn specimen_uploads() -> Uploads {
    let mut uploads = Uploads::new([
        UploadSpec::new("statement", 5 * 1024 * 1024).accept([".csv", ".txt", "text/csv"]),
        UploadSpec::new("documents", 25 * 1024 * 1024).max_entries(2),
        UploadSpec::new("scan", 2 * 1024 * 1024)
            .accept(["application/pdf", "image/*"])
            .max_entries(2),
        UploadSpec::new("locked", 1024),
    ]);
    let mut ids = 0;
    let client = |name: &str, size: u64, kind: &str| ClientEntry {
        client_ref: name.to_owned(),
        name: name.to_owned(),
        size,
        content_type: kind.to_owned(),
    };
    uploads.register(
        "documents",
        vec![
            client("invoice-2026-0918.pdf", 3_200, "application/pdf"),
            client("delivery-note.pdf", 1_800, "application/pdf"),
            client("one-too-many.pdf", 900, "application/pdf"),
        ],
        &mut ids,
    );
    // The first file is complete, the second a little over a third in.
    let _ = uploads.append(1, 0, &[0; 3_200]);
    let _ = uploads.append(2, 0, &[0; 700]);
    uploads.register(
        "scan",
        vec![
            client(
                "contract.docx",
                40_000,
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            ),
            client("contract-scan.pdf", 9 * 1024 * 1024, "application/pdf"),
        ],
        &mut ids,
    );
    uploads
}

// An extension of Bilimbi's form specimens for the theme-role colour input.
fn colour_inputs() -> Vec<crate::ColourInput> {
    use crate::{ColourChoice, ColourInput, ColourRole};
    let options = || {
        let mut choices = vec![ColourChoice::new("Use role colour", "", ColourRole::Info)];
        choices.extend(
            [
                ("info", ColourRole::Info),
                ("success", ColourRole::Success),
                ("warning", ColourRole::Warning),
                ("danger", ColourRole::Danger),
            ]
            .map(|(value, role)| ColourChoice::new(role.label(), value, role)),
        );
        choices
    };
    vec![
        ColourInput::new(
            Field::in_form("sample", "colour")
                .label("Banner colour")
                .hint("Choose a colour, or follow the server role."),
            options(),
        ),
        ColourInput::new(
            Field::in_form("sample", "colour_error")
                .label("Colour")
                .value("warning")
                .error("Choose a colour that is available."),
            options(),
        ),
        ColourInput::new(
            Field::in_form("sample", "colour_disabled")
                .label("Colour")
                .value("success")
                .disabled()
                .hint("This colour is set by your administrator."),
            options(),
        ),
        ColourInput::new(
            Field::in_form("sample", "colour_required")
                .label("Colour")
                .value("danger")
                .required(),
            options()
                .into_iter()
                .filter(|option| !option.value.is_empty())
                .collect(),
        ),
    ]
}
