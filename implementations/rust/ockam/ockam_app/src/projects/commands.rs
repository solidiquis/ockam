use std::sync::Arc;

use tauri::{async_runtime::RwLock, AppHandle, Manager, Runtime, State};
use tracing::{debug, error, info, trace, warn};

use ockam_api::address::controller_route;
use ockam_api::{cli_state::StateDirTrait, cloud::project::Project, identity::EnrollmentTicket};

use super::error::{Error, Result};
use super::State as ProjectState;
use crate::app::AppState;

// Store the user's admin projects
pub type SyncAdminProjectsState = Arc<RwLock<ProjectState>>;

pub(crate) async fn create_enrollment_ticket<R: Runtime>(
    project_id: String,
    app: AppHandle<R>,
) -> Result<EnrollmentTicket> {
    let app_state: State<'_, AppState> = app.state();
    let projects_state: State<'_, SyncAdminProjectsState> = app.state();
    let projects = projects_state.read().await;
    let project = projects
        .iter()
        .find(|p| p.id == project_id)
        .ok_or_else(|| Error::ProjectNotFound(project_id.to_owned()))?;

    debug!(?project_id, "Creating enrollment ticket via CLI");
    // TODO: How might this degrade for users who have multiple spaces and projects?
    let background_node_client = app_state.background_node_client().await;
    let hex_encoded_ticket = background_node_client
        .projects()
        .ticket(&project.name)
        .await
        .map_err(|_| Error::EnrollmentTicketFailed)?;
    serde_json::from_slice(&hex::decode(hex_encoded_ticket).map_err(|err| {
        error!(?err, "Could not hex-decode enrollment ticket");
        Error::EnrollmentTicketDecodeFailed
    })?)
    .map_err(|err| {
        error!(?err, "Could not JSON-decode enrollment ticket");
        Error::EnrollmentTicketDecodeFailed
    })
}

pub(crate) async fn refresh_projects<R: Runtime>(app: AppHandle<R>) -> Result<()> {
    info!("Refreshing projects");
    let state: State<'_, AppState> = app.state();
    if !state.is_enrolled().await.unwrap_or(false) {
        return Ok(());
    }
    let email = match state.user_email().await {
        Ok(email) => email,
        Err(_) => {
            warn!("User info is not available");
            return Ok(());
        }
    };

    let node_manager_worker = state.node_manager_worker().await;
    let projects = node_manager_worker
        .list_projects(&state.context(), &controller_route())
        .await
        .map_err(Error::ListingFailed)?
        .into_iter()
        .filter(|p| p.has_admin_with_email(&email))
        .collect::<Vec<Project>>();
    debug!("Projects fetched");
    trace!(?projects);

    let cli_projects = state.state().await.projects;
    for project in &projects {
        cli_projects
            .overwrite(&project.name, project.clone())
            .map_err(|_| Error::StateSaveFailed)?;
    }

    let project_state: State<'_, SyncAdminProjectsState> = app.state();
    let mut writer = project_state.write().await;
    *writer = projects;

    app.trigger_global(super::events::REFRESHED_PROJECTS, None);
    Ok(())
}
