use std::ops::Deref;
use std::time::Duration;

use axum::extract::Query;
use axum_extra::TypedHeader;
use color_eyre::eyre::{Context, OptionExt, eyre};
use diesel::OptionalExtension;
use diesel::{
    ExpressionMethods, HasQuery, QueryDsl,
    prelude::{AsChangeset, Insertable},
};
use diesel_async::RunQueryDsl;
use diesel_derive_enum::DbEnum;
use opentelemetry::global;
use rdkafka::message::OwnedHeaders;
use rdkafka::producer::FutureRecord;
use reqwest::StatusCode;
use sailfish::{TemplateOnce, TemplateSimple};
use serde::{Deserialize, Serialize};
use tracing::instrument;
use utoipa::ToSchema;

use crate::KafkaState;
use crate::emails::Email;
use crate::telemetry::KafkaOwnedHeaderCarrier;
use crate::{
    Placeholder,
    api::{
        auth::pool::DatabaseConnection,
        users::{User, serialize_user_id},
    },
    conditional_query,
    error::{self, Error, WithStatusCode},
    html_or_json::{HtmlOrJsonHeader, HtmlOrJsonOnce, HtmlOrJsonSimple},
    htmx::HxRequest,
    json_or_form::JsonOrForm,
    openapi_template_render, openapi_template_serialize, openapi_template_utoipa,
    schema::{offers, sql_types, users},
};

#[derive(Insertable, AsChangeset, ToSchema, Deserialize, Serialize, Debug)]
#[diesel(table_name = crate::schema::offers)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct InsertableGameOffer {
    offer_up: i32,
    for_game: i32,
    #[serde(skip)]
    made_by: i32,
}

impl Placeholder for InsertableGameOffer {
    fn placeholder() -> Self {
        Self {
            offer_up: 0,
            for_game: 1,
            made_by: 0,
        }
    }
}

#[derive(HasQuery, ToSchema, Deserialize, Serialize, Debug)]
#[diesel(table_name = crate::schema::offers)]
#[diesel(check_for_backend(diesel::pg::Pg))]
#[diesel(base_query = offers::table.inner_join(users::table))]
pub struct GameOffer {
    offer_up: i32,
    for_game: i32,
    #[diesel(embed)]
    #[schema(value_type = String)]
    #[serde(serialize_with = "serialize_user_id")]
    made_by: User,
    offer_status: OfferStatus,
}

#[derive(AsChangeset, ToSchema, Deserialize, Serialize, Debug)]
#[diesel(table_name = crate::schema::offers)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ChangesetOffer {
    offer_up: i32,
    for_game: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    offer_status: Option<OfferStatus>,
}

impl Placeholder for ChangesetOffer {
    fn placeholder() -> Self {
        Self {
            offer_up: 0,
            for_game: 1,
            offer_status: Some(OfferStatus::Accepted),
        }
    }
}

impl Placeholder for GameOffer {
    fn placeholder() -> Self {
        Self {
            offer_up: 0,
            for_game: 1,
            made_by: User::placeholder(),
            offer_status: OfferStatus::Up,
        }
    }
}

#[derive(Clone, Copy, DbEnum, ToSchema, Deserialize, Serialize, Debug, PartialEq)]
#[db_enum(existing_type_path = "sql_types::OfferStatus")]
pub enum OfferStatus {
    Up,
    Accepted,
    Rejected,
}

#[derive(TemplateOnce)]
#[template(path = "offers/all_offers.stpl")]
#[template(rm_whitespace = true, rm_newline = true)]
pub struct AllOffersTemplate {
    offers: Vec<GameOffer>,
    search_query: String,
    user_id: i32,
}

#[derive(TemplateSimple)]
#[template(path = "offers/offer.stpl")]
#[template(rm_whitespace = true, rm_newline = true)]
pub struct OfferTemplate {
    offer: GameOffer,
    editing: bool,
    user_id: i32,
}

impl Placeholder for AllOffersTemplate {
    fn placeholder() -> Self {
        Self {
            offers: vec![GameOffer::placeholder()],
            search_query: String::new(),
            user_id: 1,
        }
    }
}

impl Placeholder for OfferTemplate {
    fn placeholder() -> Self {
        Self {
            offer: GameOffer::placeholder(),
            editing: false,
            user_id: 1,
        }
    }
}

openapi_template_utoipa!(OfferTemplate);
openapi_template_serialize!(OfferTemplate, offer);
openapi_template_render!(OfferTemplate, render_placeholder, placeholder);
openapi_template_utoipa!(AllOffersTemplate);
openapi_template_serialize!(AllOffersTemplate, offers);
openapi_template_render!(AllOffersTemplate, render_placeholder, placeholder);

#[derive(Default, Deserialize, Debug)]
pub struct OfferQuery {
    // #[serde(default)]
    // pub q: String,
    #[serde(default)]
    pub uid: i32,
    #[serde(default)]
    pub status: Option<OfferStatus>,
}

#[utoipa::path(
    get,
    path = "/offers",
    tag = "Offers",
    description = "Get all the offers that you've made",
    responses(
        (status = OK, description = "Ok",
            content(
                (inline(AllOffersTemplate) = "text/html", example = AllOffersTemplate::render_placeholder),
                ([GameOffer], example = json!([GameOffer::placeholder()]))
            )
        ),
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
    ),
    security(
        ("basic_auth" = []),
        ("bearer_jwt" = []),
        ("cookie_jwt" = []),
    )
)]
#[instrument(skip(conn))]
pub async fn get_offers(
    DatabaseConnection(mut conn, _, user): DatabaseConnection,
    Query(search): Query<OfferQuery>,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
    TypedHeader(hx_request): TypedHeader<HxRequest>,
    JsonOrForm(game_offer): JsonOrForm<InsertableGameOffer>,
) -> Result<HtmlOrJsonOnce<AllOffersTemplate>, error::Error> {
    if let Some(user) = user {
        let offers = conditional_query!(
            search.uid == 0,
            query_w_uid,
            GameOffer::query(),
            GameOffer::query().filter(offers::made_by.eq(search.uid)),
            conditional_query!(
                search.status.is_none(),
                query_w_status,
                query_w_uid,
                query_w_uid.filter(offers::offer_status.eq(search.status.unwrap())),
                query_w_status
                    .load(&mut conn)
                    .await
                    .wrap_err("Failed to get updated offers list")
                    .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?
            )
        );
        // let offers = GameOffer::query()
        //     .load(&mut conn)
        //     .await
        //     .wrap_err("Failed to insert offer into database")
        //     .with_status_code(StatusCode::UNPROCESSABLE_ENTITY)?;

        Ok(HtmlOrJsonOnce(
            accept,
            hx_request,
            AllOffersTemplate {
                offers,
                search_query: String::new(),
                user_id: user.id,
            },
        ))
    } else {
        Err(eyre!("You aren't logged in")).with_status_code(StatusCode::UNAUTHORIZED)
    }
}

#[utoipa::path(
    patch,
    path = "/offers",
    tag = "Offers",
    description = "Change the offer status on the offer",
    request_body(content(
        (ChangesetOffer, example = ChangesetOffer::placeholder),
        (ChangesetOffer = "application/x-www-form-urlencoded")
    )),
    responses(
        (status = OK, description = "Ok",
            content(
                (inline(OfferTemplate) = "text/html", example = OfferTemplate::render_placeholder),
                (GameOffer, example = GameOffer::placeholder)
            )
        ),
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
    ),
    security(
        ("basic_auth" = []),
        ("bearer_jwt" = []),
        ("cookie_jwt" = []),
    )
)]
#[instrument(skip(conn))]
pub async fn patch_offer(
    DatabaseConnection(mut conn, jar, user): DatabaseConnection,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
    TypedHeader(hx_request): TypedHeader<HxRequest>,
    JsonOrForm(game_offer): JsonOrForm<ChangesetOffer>,
) -> Result<HtmlOrJsonSimple<OfferTemplate>, error::Error> {
    if let Some(user) = user {
        let offer_up = game_offer.offer_up;
        let for_game = game_offer.for_game;

        diesel::update(offers::table)
            .filter(offers::offer_up.eq(offer_up))
            .filter(offers::for_game.eq(for_game))
            .set(game_offer)
            .execute(&mut conn)
            .await
            .wrap_err("Failed to update offer in database")
            .with_status_code(StatusCode::UNPROCESSABLE_ENTITY)?;

        let offer = GameOffer::query()
            .filter(offers::offer_up.eq(offer_up))
            .filter(offers::for_game.eq(for_game))
            .first(&mut conn)
            .await
            .optional()
            .wrap_err("Failed to get created offer")
            .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or_eyre("Couldn't find that offer")
            .with_status_code(StatusCode::NOT_FOUND)?;

        Ok(HtmlOrJsonSimple(
            accept,
            hx_request,
            OfferTemplate {
                offer,
                editing: false,
                user_id: user.id,
            },
        ))
    } else {
        Err(eyre!("You aren't logged in")).with_status_code(StatusCode::UNAUTHORIZED)
    }
}

#[utoipa::path(
    post,
    path = "/offers",
    tag = "Offers",
    description = "Offer a game in exchange for another game",
    request_body(content(
        (InsertableGameOffer, example = InsertableGameOffer::placeholder),
        (InsertableGameOffer = "application/x-www-form-urlencoded")
    )),
    responses(
        (status = OK, description = "Ok",
            content(
                (inline(OfferTemplate) = "text/html", example = OfferTemplate::render_placeholder),
                (GameOffer, example = GameOffer::placeholder)
            )
        ),
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
    ),
    security(
        ("basic_auth" = []),
        ("bearer_jwt" = []),
        ("cookie_jwt" = []),
    )
)]
#[instrument(skip(conn))]
pub async fn offer_game(
    DatabaseConnection(mut conn, _, user): DatabaseConnection,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
    TypedHeader(hx_request): TypedHeader<HxRequest>,
    kafka_state: KafkaState,
    JsonOrForm(mut game_offer): JsonOrForm<InsertableGameOffer>,
) -> Result<HtmlOrJsonSimple<OfferTemplate>, error::Error> {
    if let Some(user) = user {
        let offer_up = game_offer.offer_up;
        let for_game = game_offer.for_game;
        game_offer.made_by = user.id;

        diesel::insert_into(offers::table)
            .values(game_offer)
            .execute(&mut conn)
            .await
            .wrap_err("Failed to insert offer into database")
            .with_status_code(StatusCode::UNPROCESSABLE_ENTITY)?;

        kafka_state
            .producer
            .send(
                FutureRecord::<[u8], _>::to(kafka_state.email_topic.deref())
                    .payload(
                        &postcard::to_stdvec(&Email {
                            to_id: user.id,
                            email_string: String::from("Created an offer"),
                        })
                        .wrap_err("Failed to serialize email message")
                        .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?,
                    )
                    .headers(global::get_text_map_propagator(|propagator| {
                        let mut headers = OwnedHeaders::new();
                        propagator.inject(&mut KafkaOwnedHeaderCarrier::new(&mut headers));
                        headers
                    })),
                Duration::from_secs(0),
            )
            .await
            .map_err(|e| e.0)
            .wrap_err("Failed to send email message to kafka")
            .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

        let offer = GameOffer::query()
            .filter(offers::offer_up.eq(offer_up))
            .filter(offers::for_game.eq(for_game))
            .first(&mut conn)
            .await
            .optional()
            .wrap_err("Failed to get created offer")
            .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or_eyre("Couldn't find that offer")
            .with_status_code(StatusCode::NOT_FOUND)?;

        Ok(HtmlOrJsonSimple(
            accept,
            hx_request,
            OfferTemplate {
                offer,
                editing: false,
                user_id: user.id,
            },
        ))
    } else {
        Err(eyre!("You aren't logged in")).with_status_code(StatusCode::UNAUTHORIZED)
    }
}
