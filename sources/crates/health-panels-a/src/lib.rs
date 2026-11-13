//! Health panels (a): model picker, API keys, effort, permission mode, help, settings, resume,
//! cost and the first-run wizard.

mod agents;
mod customeffort;
pub mod extra;
mod keys;
pub mod login;
mod misc;
mod model;
mod onboarding;
mod reset;
mod settings;
pub mod util;

use health_ui::{Panel, PanelKind, Services};

/// Create a panel this crate owns.
pub fn open(kind: &PanelKind, s: &Services) -> Option<Box<dyn Panel>> {
    Some(match kind {
        PanelKind::ModelPicker { query } => Box::new(model::ModelPicker::new(s, query.clone())),
        PanelKind::Keys { add } => Box::new(keys::KeysPanel::new(s, *add)),
        PanelKind::Effort => Box::new(misc::EffortPanel::new(s)),
        PanelKind::Mode => Box::new(misc::ModePanel::new(s)),
        PanelKind::Help => Box::new(misc::HelpPanel::new()),
        PanelKind::Settings => Box::new(settings::SettingsPanel::new(s)),
        PanelKind::Resume => Box::new(misc::ResumePanel::new()),
        PanelKind::Cost => Box::new(misc::CostPanel::new()),
        PanelKind::Onboarding => Box::new(onboarding::Onboarding::new(s)),
        PanelKind::CustomEffort => Box::new(customeffort::CustomEffortPanel::new(s)),
        PanelKind::Agents => Box::new(agents::AgentsPanel::new(s)),
        PanelKind::Login => Box::new(login::LoginPanel::new(s)),
        PanelKind::Reset => Box::new(reset::ResetPanel::new()),
        PanelKind::Process => Box::new(extra::ProcessPanel::new(s)),
        PanelKind::Workflow => Box::new(extra::WorkflowPanel::new(s)),
        PanelKind::Profile => Box::new(extra::ProfilePanel::new(s)),
        PanelKind::Flow => Box::new(extra::FlowPanel::new(s)),
        _ => return None,
    })
}

/// The help panel (fallback for unknown panels).
pub fn help(_s: &Services) -> Box<dyn Panel> {
    Box::new(misc::HelpPanel::new())
}
