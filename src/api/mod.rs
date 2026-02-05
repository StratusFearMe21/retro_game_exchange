pub mod auth;
pub mod games;
pub mod offers;
pub mod users;

use axum::http::StatusCode;
use color_eyre::eyre::{Context, eyre};
use diesel_async::RunQueryDsl;
use tracing::instrument;

use crate::{
    Placeholder,
    api::auth::pool::DatabaseConnection,
    error::{self, Error, WithStatusCode},
};

#[utoipa::path(
    get,
    path = "/health",
    tag = "Misc",
    description = "Checks the health of the service",
    responses(
        (status = OK, description = "Ok"),
        (status = "4XX", description = "It's your fault",
            content(
                (Error, example = Error::placeholder),
            )
        ),
        (status = "5XX", description = "We're having a skill issue",
            content(
                (Error, example = Error::placeholder),
            )
        ),
    )
)]
#[instrument(skip(conn))]
pub async fn health(
    DatabaseConnection(mut conn, _, user): DatabaseConnection,
) -> Result<(), error::Error> {
    let rows = diesel::sql_query("SELECT current_timestamp - pg_postmaster_start_time()")
        .execute(&mut conn)
        .await
        .wrap_err("Failed to get current timestamp list")
        .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

    if rows < 1 {
        return Err(eyre!("Postgres returned no rows for current uptime"))
            .with_status_code(StatusCode::INTERNAL_SERVER_ERROR);
    }

    Ok(())
}
