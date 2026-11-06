//! Bootstrap, first-run setup, sign-in and staff commands (E04-T01, E05-T01/T02).
//! Thin adapters: parse, call the service, map. Authorization happens in the services.

use crate::dto::{
    BootstrapState, CreateWorkspaceInput, CreatedWorkspaceDto, NewStaffInput, SessionDto, SignInUser,
    StaffDto, StaffUpdateInput, WorkspaceDto, rate_in,
};
use crate::services::Session;
use crate::{AppServices, IpcError};
use pos_application::context::{Actor, AppError};
use pos_application::queries::SetupQueries;
use pos_application::records::SessionEnd;
use pos_application::setup::{CreateWorkspace, NewTaxRate};
use pos_application::staff::NewStaff;
use pos_domain::UserId;
use pos_domain::staff::Pin;
use tauri::State;

pub(crate) fn err(e: AppError) -> IpcError {
    IpcError::from_app(&e)
}

fn pin(s: &str) -> Result<Pin, IpcError> {
    Pin::parse(s).map_err(|e| err(e.into()))
}

fn session_dto(s: &Session) -> SessionDto {
    SessionDto {
        user_id: s.actor.user_id,
        display_name: s.display_name.clone(),
        role: s.actor.role,
        must_change_pin: s.must_change_pin,
    }
}

/// What the shell needs before anything else: set up yet? who is signed in? No session
/// required.
#[tauri::command]
#[specta::specta]
pub async fn app_bootstrap_state(services: State<'_, AppServices>) -> Result<BootstrapState, IpcError> {
    let app = services.app()?;
    let summary = app.queries.workspace_summary().await.map_err(|e| err(e.into()))?;
    let session = services.session().await;
    Ok(BootstrapState {
        setup_complete: summary.is_some(),
        workspace: summary.map(|w| WorkspaceDto {
            business_name: w.business_name,
            register_name: w.register_name,
            register_code: w.register_code,
            currency_code: w.currency_code,
            mode: w.mode.as_str().into(),
        }),
        session: session.as_ref().map(session_dto),
    })
}

/// First run (E04-T01). Refused once setup has completed. Does not sign anyone in: the UI
/// signs the owner in with `auth_login`, so every session is a real, audited sign-in.
#[tauri::command]
#[specta::specta]
pub async fn setup_create_workspace(
    services: State<'_, AppServices>,
    input: CreateWorkspaceInput,
) -> Result<CreatedWorkspaceDto, IpcError> {
    let app = services.app()?;
    let tax_rates = input
        .tax_rates
        .into_iter()
        .map(|r| {
            Ok(NewTaxRate {
                name: r.name,
                rate: rate_in("ratePpm", r.rate_ppm)?,
            })
        })
        .collect::<Result<Vec<_>, IpcError>>()?;
    let ws = app
        .setup
        .create_workspace(CreateWorkspace {
            business_name: input.business_name,
            legal_name: input.legal_name,
            address: serde_json::json!({ "lines": input.address_lines }),
            phone: input.phone,
            email: input.email,
            country_code: input.country_code,
            timezone: input.timezone,
            currency_code: input.currency_code,
            prices_include_tax: input.prices_include_tax,
            location_name: input.location_name,
            register_name: input.register_name,
            owner_name: input.owner_name,
            owner_pin: pin(&input.owner_pin)?,
            tax_rates,
        })
        .await
        .map_err(err)?;
    pos_observability::set_context("register_id", ws.register.register_id.to_string());
    Ok(CreatedWorkspaceDto {
        owner_user_id: ws.owner_user_id,
        register_code: ws.register_code,
    })
}

/// Active staff for the PIN pad. No session required.
#[tauri::command]
#[specta::specta]
pub async fn auth_list_users(services: State<'_, AppServices>) -> Result<Vec<SignInUser>, IpcError> {
    services.context().await?; // only after setup
    let users = services.app()?.auth.sign_in_list().await.map_err(err)?;
    Ok(users
        .into_iter()
        .map(|u| SignInUser {
            user_id: u.user_id,
            display_name: u.display_name,
            role: u.role,
        })
        .collect())
}

/// PIN sign-in with lockout (E05-T02). On success it replaces any current session; a failed
/// attempt leaves the current session untouched.
#[tauri::command]
#[specta::specta]
pub async fn auth_login(
    services: State<'_, AppServices>,
    user_id: UserId,
    pin: String,
) -> Result<SessionDto, IpcError> {
    let current = services.context().await?;
    let pin = self::pin(&pin)?;
    let app = services.app()?;
    let mut anonymous = current.clone();
    anonymous.actor = None;
    let grant = app.auth.login(&anonymous, user_id, pin).await.map_err(err)?;
    // Verified: now end the previous session (audited as a user switch).
    if current.actor.is_some() {
        app.auth
            .end_session(&current, SessionEnd::SwitchUser)
            .await
            .map_err(err)?;
    }
    let session = Session {
        actor: Actor {
            user_id: grant.user_id,
            role: grant.role,
            session_id: grant.session_id,
        },
        display_name: grant.display_name,
        must_change_pin: grant.must_change_pin,
    };
    services.set_session(Some(session.clone())).await;
    Ok(session_dto(&session))
}

#[tauri::command]
#[specta::specta]
pub async fn auth_logout(services: State<'_, AppServices>) -> Result<(), IpcError> {
    let ctx = services.context().await?;
    services
        .app()?
        .auth
        .end_session(&ctx, SessionEnd::Logout)
        .await
        .map_err(err)?;
    services.set_session(None).await;
    Ok(())
}

/// Changes the signed-in user's own PIN (required after an owner reset).
#[tauri::command]
#[specta::specta]
pub async fn auth_change_pin(
    services: State<'_, AppServices>,
    current_pin: String,
    new_pin: String,
) -> Result<SessionDto, IpcError> {
    let ctx = services.context().await?;
    let record = services
        .app()?
        .staff
        .change_own_pin(&ctx, pin(&current_pin)?, pin(&new_pin)?)
        .await
        .map_err(err)?;
    let mut session = services
        .session()
        .await
        .ok_or_else(|| err(AppError::SessionLocked))?;
    session.must_change_pin = record.must_change_pin;
    services.set_session(Some(session.clone())).await;
    Ok(session_dto(&session))
}

#[tauri::command]
#[specta::specta]
pub async fn staff_list(services: State<'_, AppServices>) -> Result<Vec<StaffDto>, IpcError> {
    let ctx = services.context().await?;
    let staff = services.app()?.staff.list(&ctx).await.map_err(err)?;
    staff.iter().map(StaffDto::from_record).collect()
}

#[tauri::command]
#[specta::specta]
pub async fn staff_create(
    services: State<'_, AppServices>,
    input: NewStaffInput,
) -> Result<StaffDto, IpcError> {
    let ctx = services.context().await?;
    let record = services
        .app()?
        .staff
        .create(
            &ctx,
            NewStaff {
                display_name: input.display_name,
                role: input.role,
                pin: pin(&input.pin)?,
            },
        )
        .await
        .map_err(err)?;
    StaffDto::from_record(&record)
}

#[tauri::command]
#[specta::specta]
pub async fn staff_update(
    services: State<'_, AppServices>,
    input: StaffUpdateInput,
) -> Result<StaffDto, IpcError> {
    let ctx = services.context().await?;
    let record = services
        .app()?
        .staff
        .update(
            &ctx,
            input.user_id,
            u64::from(input.base_version),
            input.display_name,
            input.role,
        )
        .await
        .map_err(err)?;
    StaffDto::from_record(&record)
}

#[tauri::command]
#[specta::specta]
pub async fn staff_deactivate(
    services: State<'_, AppServices>,
    user_id: UserId,
    base_version: u32,
) -> Result<StaffDto, IpcError> {
    let ctx = services.context().await?;
    let record = services
        .app()?
        .staff
        .deactivate(&ctx, user_id, u64::from(base_version))
        .await
        .map_err(err)?;
    StaffDto::from_record(&record)
}

/// Sets a temporary PIN; the user must change it at next sign-in.
#[tauri::command]
#[specta::specta]
pub async fn staff_reset_pin(
    services: State<'_, AppServices>,
    user_id: UserId,
    base_version: u32,
    pin: String,
) -> Result<StaffDto, IpcError> {
    let ctx = services.context().await?;
    let record = services
        .app()?
        .staff
        .reset_pin(&ctx, user_id, u64::from(base_version), self::pin(&pin)?)
        .await
        .map_err(err)?;
    StaffDto::from_record(&record)
}
