use color_eyre::eyre::{self, Context, OptionExt};
use rdkafka::{Message, message::OwnedMessage};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Email {
    pub to_id: i32,
    pub email_string: String,
}

pub async fn process_message(message: OwnedMessage) -> eyre::Result<()> {
    let email: Email = postcard::from_bytes(
        message
            .payload()
            .ok_or_eyre("The message contained no payload")?,
    )
    .wrap_err("Failed to deserialize email message")?;

    tracing::info!(?email, "Sending email");

    Ok(())
}
