//! The feedback specimen: flash, alert, panel notice, connection banners, the
//! modal and confirmation dialogs and the tab strip on one page, served
//! unauthenticated at `/assets/ui-feedback.html`. It is the screenshot baseline
//! for those components, not the Design Library. The dialogs are `<template>`
//! fragments rendered by the same macros; `static/ui-feedback-driver.js` stands in
//! for the server by inserting and removing them.
use crate::section::sections;
use crate::{FlashMap, InstanceBadge, InstanceBanner, InstanceStatus, StatusKind, Tab};
use askama::Template;

/// The page is cut into sections, each its own template; see `section`.
#[derive(Template)]
#[template(path = "ui_feedback_specimen.html")]
pub struct FeedbackSpecimen {
    confirmation_reason: crate::Html,
    tabs_links: Vec<Tab<'static>>,
    tabs_narrow: Vec<Tab<'static>>,
    tabs_buttons: Vec<Tab<'static>>,
    tabs_single: Vec<Tab<'static>>,
    long_paragraphs: Vec<usize>,
    flash_in_dialog: FlashMap<'static>,
    instance_statuses: Vec<InstanceStatus<'static>>,
    instance_badges: Vec<InstanceBadge<'static>>,
    instance_banners: Vec<InstanceBanner<'static>>,
}

sections! {
    FeedbackSpecimen {
        flash_group: "ui_feedback_specimen/flash_group.html",
        flash: "ui_feedback_specimen/flash.html",
        alerts: "ui_feedback_specimen/alerts.html",
        instances: "ui_feedback_specimen/instances.html",
        connection: "ui_feedback_specimen/connection.html",
        dialogs: "ui_feedback_specimen/dialogs.html",
        tab_strips: "ui_feedback_specimen/tab_strips.html",
        flash_templates: "ui_feedback_specimen/flash_templates.html",
        modal_templates: "ui_feedback_specimen/modal_templates.html",
        confirm_templates: "ui_feedback_specimen/confirm_templates.html",
    }
}

impl FeedbackSpecimen {
    pub fn new() -> Self {
        let link = |label| Tab::link("#component-tabs", label, label == "Overview");
        Self {
            confirmation_reason: crate::Html::render(&crate::Field::named("reason").id("design-library-confirm-reason").label("Reason (optional)").kind(crate::InputKind::Textarea)).expect("confirmation reason specimen"),
            tabs_links: [
                "Overview",
                "History",
                "Settings",
                "Recent activity",
                "Access history",
            ]
            .map(link)
            .into(),
            tabs_narrow: [
                "Overview",
                "History",
                "Settings",
                "Recent activity",
                "Access history",
            ]
            .map(link)
            .into(),
            tabs_buttons: vec![
                Tab::button("switch-tab", "overview", "Overview", true),
                Tab::button("switch-tab", "history", "History", false),
                Tab::button("switch-tab", "settings", "Settings", false),
            ],
            tabs_single: vec![Tab::link("#component-tabs", "Overview", true)],
            long_paragraphs: (1..=18).collect(),
            flash_in_dialog: FlashMap {
                error: "The changes could not be saved.",
                info: "Nothing else changed.",
                ..FlashMap::default()
            },
            instance_statuses: vec![
                InstanceStatus { id: "", tone: StatusKind::Info, role: "development", role_label: "Dev", label: "Dev", title: "You are on Dev.", editable: true },
                InstanceStatus { id: "", tone: StatusKind::Warning, role: "staging", role_label: "Staging", label: "Training", title: "You are on Training (Staging).", editable: false },
                InstanceStatus { id: "", tone: StatusKind::Error, role: "production", role_label: "Production", label: "Production", title: "You are on Production.", editable: false },
            ],
            instance_badges: vec![
                InstanceBadge {
                    tone: StatusKind::Info,
                    role: "development",
                    label: "Dev",
                    title: "You are on Dev. This is a development copy. Changes here do not affect production.",
                },
                InstanceBadge {
                    tone: StatusKind::Warning,
                    role: "staging",
                    label: "SBG Staging",
                    title: "You are on SBG Staging. This is a test copy. Changes here do not affect production.",
                },
            ],
            instance_banners: vec![
                InstanceBanner {
                    id: "design-library-instance-development",
                    tone: StatusKind::Info,
                    role: "development",
                    label: "Dev",
                    message: "This is a development copy. Changes here do not affect production.",
                    class: "",
                },
                InstanceBanner {
                    id: "design-library-instance-staging",
                    tone: StatusKind::Warning,
                    role: "staging",
                    label: "SBG Staging",
                    message: "This is a test copy. Changes here do not affect production.",
                    class: "",
                },
                InstanceBanner {
                    id: "design-library-instance-production",
                    tone: StatusKind::Error,
                    role: "production",
                    label: "SBG Production",
                    message: "This is the live system. Changes here change real data.",
                    class: "",
                },
            ],
        }
    }
}

impl Default for FeedbackSpecimen {
    fn default() -> Self {
        Self::new()
    }
}
