use color_eyre::eyre::{Context, OptionExt};

use crate::{
    api::offers::OfferStatus,
    kafka::{JobError, JobErrorStatus, KafkaMessage, WithErrorStatus},
};

pub async fn process_message(message: Option<&[u8]>) -> Result<(), JobError> {
    let message: KafkaMessage = postcard::from_bytes(
        message
            .ok_or_eyre("The message contained no payload")
            .with_error_status(JobErrorStatus::NotRetryable)?,
    )
    .wrap_err("Failed to deserialize email message")
    .with_error_status(JobErrorStatus::NotRetryable)?;

    match message {
        KafkaMessage::UserInformationChanged(user) => {
            tracing::info!(
                to = user.id,
                subject_line = format_args!("Your information changed"),
                "Hi {}, We just wanted to let you know\
                that your user information was changed.\
                You may unsafely ignore this email if that wasn't you",
                user.username
            );
        }
        KafkaMessage::VideoGameOfferChanged(offer) => {
            tracing::info!(
                to = offer.made_by,
                subject_line = match offer.offer_status {
                    OfferStatus::Up => "You created an offer",
                    OfferStatus::Accepted => "Your offer was accepted",
                    OfferStatus::Rejected => "Your offer was rejected",
                },
                "Your offer is to trade game #{} for game #{}.\
                The status of this offer has changed.",
                offer.offer_up,
                offer.for_game
            );
            tracing::info!(
                to = offer.made_to.id,
                subject_line = match offer.offer_status {
                    OfferStatus::Up => "Somebody made an offer to you",
                    OfferStatus::Accepted => "You accepted an offer",
                    OfferStatus::Rejected => "You rejected an offer",
                },
                "Someone wants to offer game #{} for game #{}.\
                The status of this offer has changed.",
                offer.offer_up,
                offer.for_game
            );
        }
    }

    Ok(())

    // Err(eyre!("Uh oh, stinky")).with_error_status(JobErrorStatus::Retryable)
}
