use std::borrow::Cow;

use color_eyre::eyre::{Context, OptionExt};
use serde::{Deserialize, Serialize};

use crate::kafka::{JobError, JobErrorStatus, WithErrorStatus};

#[derive(Debug, Serialize, Deserialize)]
pub struct Email<'a> {
    pub to_id: i32,
    pub email_string: Cow<'a, str>,
}

pub async fn process_message(message: Option<&[u8]>) -> Result<(), JobError> {
    let email: Email = postcard::from_bytes(
        message
            .ok_or_eyre("The message contained no payload")
            .with_error_status(JobErrorStatus::NotRetryable)?,
    )
    .wrap_err("Failed to deserialize email message")
    .with_error_status(JobErrorStatus::NotRetryable)?;

    tracing::info!(?email, "Sending email");

    Ok(())

    // Err(eyre!("Uh oh, stinky")).with_error_status(JobErrorStatus::Retryable)
}
