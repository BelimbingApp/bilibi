//! The portal's own pages (ADR 16), served to a signed-in customer with its
//! `PortalScope`: the home (`/portal`) and the customer's account
//! (`/portal/account`). Both ask for `PORTAL_ACCOUNT` again and read only what
//! the scope names. Bilimbi has no customer portal; the pages are built from
//! the shared header, card and facts models.
use crate::{facade::CustomerAccess, ACCOUNT_PATH, PORTAL_ACCOUNT};
use bilibi_database::{Db, Error, Result};
use bilibi_tenancy::PortalScope;
use bilibi_ui::{
    page::{Page, PageBody, PageFuture, PageRequest},
    ActionLink, Card, Fact, Facts, Header, Html,
};
use std::sync::Arc;

pub struct Pages {
    pub db: Db,
    pub access: Arc<CustomerAccess>,
}

fn render(template: &impl askama::Template) -> Result<Html> {
    Html::render(template).map_err(|_| Error::Internal)
}

impl Pages {
    async fn authorize(&self, portal: &PortalScope) -> Result<()> {
        let client = self.db.get().await?;
        bilibi_authz::authorize_portal(&**client, portal, PORTAL_ACCOUNT).await
    }
    /// The company the customer signs in through, by its display name.
    async fn company(&self, portal: &PortalScope) -> Result<Option<String>> {
        let Some(organisation) = portal.organisation() else {
            return Ok(None);
        };
        let client = self.db.get().await?;
        Ok(
            bilibi_company::identity(&**client, portal.scope(), organisation.get())
                .await?
                .map(|identity| identity.display_name),
        )
    }
    async fn home(&self, portal: &PortalScope) -> Result<Html> {
        let scope = portal.scope();
        let title = self
            .access
            .site_of(portal)
            .await?
            .map_or_else(|| scope.tenant_name.clone(), |site| site.title);
        let mut body = render(&Header {
            title: Html::text(&format!("Welcome, {}", scope.user_name)),
            subtitle: "Your customer portal",
            ..Header::default()
        })?;
        let mut card = Html::text(&format!(
            "You are signed in to {title} as {}. Use the menu above to open what {title} shares with you.",
            scope.email
        ));
        card.push(&render(&ActionLink {
            id: "portal-home-account",
            icon: "user-circle",
            title: "My account",
            href: ACCOUNT_PATH,
            body: Html::text("My account"),
            ..ActionLink::default()
        })?);
        body.push(&render(&Card {
            id: "portal-home-welcome",
            body: card,
            ..Card::default()
        })?);
        Ok(body)
    }
    async fn account(&self, portal: &PortalScope) -> Result<Html> {
        let scope = portal.scope();
        let company = self.company(portal).await?;
        let site = self.access.site_of(portal).await?;
        let mut body = render(&Header {
            title: Html::text("My account"),
            subtitle: "The details you sign in with",
            ..Header::default()
        })?;
        let mut items = vec![
            Fact {
                title: "Name",
                id: "portal-account-name",
                value: Html::text(&scope.user_name),
            },
            Fact {
                title: "Email",
                id: "portal-account-email",
                value: Html::text(&scope.email),
            },
        ];
        items.push(Fact {
            title: "Company",
            id: "portal-account-company",
            value: Html::text(company.as_deref().unwrap_or("None (private customer)")),
        });
        let mut card = render(&Facts {
            id: "portal-account-facts",
            items,
        })?;
        // Changing the password goes through the emailed reset link, which
        // proves the holder of the address as an in-place form would not.
        if let Some(site) = site {
            let forgot = format!("/portal/forgot/{}", site.address);
            card.push(&render(&ActionLink {
                id: "portal-account-password",
                icon: "key",
                title: "Change my password",
                href: &forgot,
                body: Html::text("Change my password"),
                ..ActionLink::default()
            })?);
        }
        body.push(&render(&Card {
            id: "portal-account",
            body: card,
            ..Card::default()
        })?);
        Ok(body)
    }
}

impl Page<PortalScope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "home" => Some("Home"),
            "account" => Some("My account"),
            _ => None,
        }
    }
    fn render<'a>(&'a self, request: PageRequest<'a, PortalScope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let portal = request.scope.ok_or(Error::Unauthorized)?;
            self.authorize(portal).await?;
            let (id, body) = match request.operation {
                "home" => ("portal-home-page", self.home(portal).await?),
                "account" => ("portal-account-page", self.account(portal).await?),
                _ => return Err(Error::NotFound),
            };
            let html = render(&bilibi_ui::Page {
                id,
                variant: bilibi_ui::PageVariant::Detail,
                body,
                ..bilibi_ui::Page::default()
            })?;
            Ok(PageBody {
                html: html.as_str().to_owned(),
                live: false,
                title: None,
            })
        })
    }
}
