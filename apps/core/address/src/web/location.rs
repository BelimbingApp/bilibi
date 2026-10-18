//! The location fieldset every address form shares (Bilimbi
//! `LocationFields`): create, the detail page's grouped Apply and a panel's
//! create-and-attach form. The cascade is `AddressService::suggest`; this file
//! only holds a form's location draft and draws its four controls.
use super::*;
use crate::{Auto, FieldError, Location, Options};
use bilibi_ui::{error_sentence, Choice, Combobox, Field, InputKind};

/// DOM ids: `country` and `admin1` as each caller names them (they differ in
/// the source), the rest derived from `prefix`.
pub(super) struct Ids {
    pub prefix: &'static str,
    pub country: &'static str,
    pub admin1: &'static str,
}
/// The nouns a caller puts on the labels.
pub(super) struct Labels {
    pub admin1: &'static str,
    pub admin1_prompt: &'static str,
    pub postcode: &'static str,
    pub locality: &'static str,
}
pub(super) const LABELS: Labels = Labels {
    admin1: "State or province",
    admin1_prompt: "Choose a division",
    postcode: "Postcode",
    locality: "Locality",
};

#[derive(Template)]
#[template(path = "location_fields.html")]
struct View<'a> {
    prefix: &'a str,
    country: Combobox,
    admin1: Field,
    postcode: Field,
    locality: Field,
    postcodes: &'a [String],
    localities: &'a [String],
}

pub(super) struct LocationDraft {
    pub location: Location,
    pub auto: Auto,
    options: Options,
}
impl LocationDraft {
    pub async fn new(addresses: &AddressService, location: Location) -> Result<Self> {
        let options = addresses.options(&location).await?;
        Ok(Self {
            location,
            auto: Auto::default(),
            options,
        })
    }
    /// The posted location (`form[country_iso]` …). A disabled control posts
    /// nothing, which reads as blank.
    pub fn posted(fields: &HashMap<String, String>) -> Location {
        let get = |key: &str| fields.get(key).cloned().unwrap_or_default();
        Location {
            country_iso: get("country_iso"),
            admin1_code: get("admin1_code"),
            postcode: get("postcode"),
            locality: get("locality"),
        }
    }
    /// Apply one round of the cascade to the posted values.
    pub async fn change(&mut self, addresses: &AddressService, incoming: &Location) -> Result<()> {
        let (next, auto) = addresses
            .suggest(incoming, &self.location, self.auto)
            .await?;
        self.options = addresses.options(&next).await?;
        self.location = next;
        self.auto = auto;
        Ok(())
    }
    pub fn render(
        &self,
        form: &str,
        ids: &Ids,
        labels: &Labels,
        countries: &[Choice],
        errors: &[FieldError],
    ) -> Result<Html> {
        // Each error reads as a sentence that names its field.
        let errors_for = |key: &str, label: &str| -> Vec<String> {
            errors
                .iter()
                .filter(|e| e.field == key)
                .map(|e| error_sentence(label, &e.message))
                .collect()
        };
        let country_blank = self.location.country_iso.trim().is_empty();
        let mut country = Combobox::new(ids.country, format!("{form}[country_iso]"))
            .label("Country")
            .value(&self.location.country_iso)
            .options(countries.to_vec())
            .errors(errors_for("country_iso", "Country"));
        country.placeholder = "Choose a country".into();
        // One line says why the three fields after the country are off,
        // instead of three grey boxes that do not.
        if country_blank {
            country = country.hint("Postcode, state and locality open once you pick a country.");
        }
        let admin1_options: Vec<Choice> = self
            .options
            .admin1
            .iter()
            .map(|(label, code)| Choice::new(label, code))
            .collect();
        let mut admin1 = Field::in_form(form, "admin1_code")
            .id(ids.admin1)
            .kind(InputKind::Select)
            .label(labels.admin1)
            .prompt(labels.admin1_prompt)
            .value(&self.location.admin1_code)
            .errors(errors_for("admin1_code", labels.admin1));
        if admin1_options.is_empty() {
            admin1 = admin1.disabled();
            if !country_blank {
                admin1 = admin1.hint("This country has no states or provinces.");
            }
        } else if self.auto.admin1_code {
            admin1 = admin1.hint("Suggested from postcode");
        }
        admin1 = admin1.options(admin1_options);
        let text = |key: &str, id: String, label: &str, value: &str, list: String| {
            let mut field = Field::in_form(form, key)
                .id(id)
                .label(label)
                .value(value)
                .attr("list", &list)
                .attr("data-debounce", "300")
                .errors(errors_for(key, label));
            field.attrs.maxlength = Some("255".into());
            if country_blank {
                field = field.disabled();
            }
            field
        };
        render(View {
            prefix: ids.prefix,
            country,
            admin1,
            postcode: text(
                "postcode",
                format!("{}-postcode", ids.prefix),
                labels.postcode,
                &self.location.postcode,
                format!("{}-postcode-options", ids.prefix),
            ),
            locality: {
                let locality = text(
                    "locality",
                    format!("{}-locality", ids.prefix),
                    labels.locality,
                    &self.location.locality,
                    format!("{}-locality-options", ids.prefix),
                );
                if self.auto.locality {
                    locality.hint("Suggested from postcode")
                } else {
                    locality
                }
            },
            postcodes: &self.options.postcodes,
            localities: &self.options.localities,
        })
    }
}
