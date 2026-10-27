//! Bilimbi's Design Library, composed from the production UI primitives.
//! This adapter is separate from UI to keep UI's identity-free dependency graph.
mod controls;
mod state;
mod view;
use bilibi_database::Error;
use bilibi_module_registry::{
    contributions::{Access, Method, Route, ROUTES},
    Contribution, Layer, Module, Provenance, Service,
};
use bilibi_tenancy::Scope;
use bilibi_ui::page::{
    LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
    PageFuture, PageRequest, PageService, Uploads,
};
use std::sync::Arc;

pub const VIEW: &str = "admin.system.design-library.view";
const BASE: &str = "/system/design-library";
struct Library {
    display: Arc<bilibi_datetime::DisplayPolicy>,
    db: bilibi_database::Db,
}
impl Page<Scope, Error> for Library {
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut state = state::State::new(request.operation, request.query)?;
            state.csrf = scope.csrf.clone();
            let client = self.db.get().await.map_err(Error::from)?;
            state.display = self
                .display
                .display(&**client, scope, None, "en-MY")
                .await?;
            if request.operation == "submit" {
                return Ok(PageBody {
                    html: String::new(),
                    live: false,
                    title: None,
                });
            }
            Ok(PageBody {
                html: view::View { state: &state }
                    .body()
                    .map_err(|_| Error::Internal)?,
                live: true,
                title: None,
            })
        })
    }
    fn title(&self, operation: &str) -> Option<&'static str> {
        Some(match operation {
            "components" | "submit" => "Components · Design Library",
            "theme" => "Theme · Design Library",
            "graphic" => "Graphic · Design Library",
            _ => "Design Spec · Design Library",
        })
    }
    fn redirect(&self, operation: &str) -> Option<&'static str> {
        (operation == "submit").then_some("/system/design-library/components?specimen=requested")
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut state = state::State::new(request.operation, request.query)?;
            state.csrf = scope.csrf.clone();
            let client = self.db.get().await.map_err(Error::from)?;
            state.display = self
                .display
                .display(&**client, scope, None, "en-MY")
                .await?;
            let state: Box<dyn LiveState<Scope, Error>> = Box::new(Mounted {
                state,
                display: self.display.clone(),
                db: self.db.clone(),
            });
            Ok(Some(state))
        })
    }
}
struct Mounted {
    state: state::State,
    display: Arc<bilibi_datetime::DisplayPolicy>,
    db: bilibi_database::Db,
}
impl LiveState<Scope, Error> for Mounted {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let client = self.db.get().await.map_err(Error::from)?;
            self.state.display = self
                .display
                .display(&**client, request.scope, None, "en-MY")
                .await?;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(params) => self.state.params(params)?,
                LiveInput::Event(event) => self.state.event(event)?,
            }
            Ok(LiveBody {
                html: view::View { state: &self.state }
                    .body()
                    .map_err(|_| Error::Internal)?,
                query: self.state.query(),
                notice: String::new(),
                redirect: self.state.redirect.take(),
                ..LiveBody::default()
            })
        })
    }
    fn uploads(&mut self) -> Option<&mut Uploads> {
        Some(&mut self.state.uploads)
    }
}
inventory::submit! {
    Module { id:"base/design_library", layer:Layer::Base,
        dependencies:&["base/module_registry","base/ui","base/database","base/tenancy","base/authz","base/datetime","base/menu"],
        migrations:&[], services:&[Service { id:"base/design_library.pages", factory:|services| {
            Ok(Arc::new(PageService::<Scope,Error>(Arc::new(Library {
                display:services.get("base/datetime.display")?,
                db:services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
            }))))
        }}],
    }
}
macro_rules! route {
    ($id:literal,$method:ident,$path:literal,$op:literal) => {
        inventory::submit! { Contribution::new(ROUTES,concat!("design_library.",$id),Provenance {
            module:"base/design_library", source:file!(),line:line!()
        }, &Route {method:Method::$method,path:$path,operation:$op,service:"base/design_library.pages",access:Access::Capability(VIEW),framed:true}) }
    };
}
route!("theme", Get, "/system/design-library", "theme");
route!(
    "components",
    Get,
    "/system/design-library/components",
    "components"
);
route!(
    "spec",
    Get,
    "/system/design-library/design-spec",
    "design_spec"
);
route!("graphic", Get, "/system/design-library/graphic", "graphic");
route!(
    "submit",
    Post,
    "/system/design-library/components/specimen-request",
    "submit"
);
inventory::submit! { Contribution::new(bilibi_authz::VOCABULARY,"design_library.vocabulary",Provenance {
    module:"base/design_library", source:file!(),line:line!()
}, &bilibi_authz::VocabularyContribution {
    domains:&[], verbs:&[], capabilities:&[bilibi_authz::CapabilityDefinition::tenant(VIEW)],
    roles:&[bilibi_authz::RoleDefinition {code:"system_viewer",name:"System Viewer",description:"System review screens",grant_all:false,capabilities:&[VIEW]}],system_principals:&[],
}) }
macro_rules! item {
    ($id:literal,$label:literal,$path:literal,$order:literal) => {
        inventory::submit! { Contribution::new(bilibi_menu::ITEMS,concat!("design_library.menu.",$id),Provenance {
            module:"base/design_library",source:file!(),line:line!()
        }, &bilibi_menu::MenuItem::page(concat!("admin.system.design-library.",$id),$label,$path)
            .under("admin.system.design-library").icon("iconoir-design-nib").requires(bilibi_menu::Requirement::Capability(VIEW)).order($order)) }
    }
}
inventory::submit! { Contribution::new(bilibi_menu::ITEMS,"design_library.menu",Provenance {
    module:"base/design_library",source:file!(),line:line!()
}, &bilibi_menu::MenuItem::container("admin.system.design-library","Design Library")
    .under("admin.system").icon("paint-brush").requires(bilibi_menu::Requirement::Capability(VIEW)).order(95)) }
item!(
    "components",
    "Components",
    "/system/design-library/components",
    0
);
item!(
    "design-spec",
    "Design Spec",
    "/system/design-library/design-spec",
    1
);
item!("graphic", "Graphic", "/system/design-library/graphic", 2);
item!("theme", "Theme", "/system/design-library", 3);
