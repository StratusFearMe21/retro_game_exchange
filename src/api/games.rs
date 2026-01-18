use axum::{
    Json,
    extract::{Path, Query},
    http::StatusCode,
};
use axum_extra::TypedHeader;
use color_eyre::eyre::{Context, eyre};
use diesel::{ExpressionMethods, HasQuery, QueryDsl, prelude::*};
use diesel_async::RunQueryDsl;
use diesel_derive_enum::DbEnum;
use sailfish::{TemplateOnce, TemplateSimple};
use serde::{Deserialize, Serialize};
use tracing::instrument;
use utoipa::ToSchema;

use crate::{
    Placeholder,
    api::auth::{User, pool::DatabaseConnection},
    error::{self, Error, WithStatusCode},
    html_or_json::{HtmlOrJsonHeader, HtmlOrJsonOnce, HtmlOrJsonSimple},
    json_or_form::JsonOrForm,
    openapi_template,
    schema::{games, sql_types, users},
};

#[derive(Insertable, AsChangeset, ToSchema, Deserialize, Serialize, Debug)]
#[diesel(table_name = crate::schema::games)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct InsertableGame {
    name: String,
    #[serde(skip)]
    owned_by: i32,
    #[diesel(treat_none_as_null = true)]
    publisher: Option<String>,
    #[diesel(treat_none_as_null = true)]
    #[schema(minimum = 0, maximum = 65535)]
    year: Option<i16>,
    #[diesel(treat_none_as_null = true)]
    platform: Option<String>,
    #[diesel(treat_none_as_null = true)]
    condition: Option<Condition>,
}

#[derive(AsChangeset, ToSchema, Deserialize, Serialize, Debug)]
#[diesel(table_name = crate::schema::games)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ChangesetGame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "::serde_with::rust::double_option"
    )]
    publisher: Option<Option<String>>,
    #[schema(minimum = 0, maximum = 65535)]
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "::serde_with::rust::double_option"
    )]
    year: Option<Option<i16>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "::serde_with::rust::double_option"
    )]
    platform: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "::serde_with::rust::double_option"
    )]
    condition: Option<Option<Condition>>,
}

#[derive(HasQuery, ToSchema, Deserialize, Serialize, Debug, Default)]
#[diesel(table_name = crate::schema::games)]
#[diesel(check_for_backend(diesel::pg::Pg))]
#[diesel(base_query = games::table.inner_join(users::table))]
pub struct GameModel {
    id: i32,
    name: String,
    publisher: Option<String>,
    #[schema(minimum = 0, maximum = 65535)]
    year: Option<i16>,
    platform: Option<String>,
    condition: Option<Condition>,
    #[diesel(embed)]
    user: User,
}

#[derive(DbEnum, ToSchema, Deserialize, Serialize, Debug, PartialEq)]
#[db_enum(existing_type_path = "sql_types::Condition")]
pub enum Condition {
    Mint,
    Good,
    Fair,
    Poor,
}

impl Placeholder for InsertableGame {
    fn placeholder() -> Self {
        Self {
            name: "Starfield".to_owned(),
            owned_by: 0,
            publisher: Some("Bethesda".to_owned()),
            year: Some(2023),
            platform: Some("PC".to_owned()),
            condition: Some(Condition::Mint),
        }
    }
}

impl Placeholder for ChangesetGame {
    fn placeholder() -> Self {
        Self {
            name: Some("Starfield".to_owned()),
            publisher: Some(Some("Bethesda".to_owned())),
            year: Some(Some(2023)),
            platform: Some(Some("PC".to_owned())),
            condition: Some(Some(Condition::Mint)),
        }
    }
}

impl Placeholder for GameModel {
    fn placeholder() -> Self {
        Self {
            id: 1,
            name: "Starfield".to_owned(),
            publisher: Some("Bethesda".to_owned()),
            year: Some(2023),
            platform: Some("PC".to_owned()),
            condition: Some(Condition::Mint),
            user: User::placeholder(),
        }
    }
}

#[derive(TemplateOnce)]
#[template(path = "games/all_games.stpl")]
#[template(rm_whitespace = true, rm_newline = true)]
pub struct AllGamesTemplate {
    games: Vec<GameModel>,
    user_id: i32,
}

#[derive(TemplateSimple)]
#[template(path = "games/game.stpl")]
#[template(rm_whitespace = true, rm_newline = true)]
pub struct GameTemplate {
    game: GameModel,
    editing: bool,
    user_id: i32,
}

impl Placeholder for AllGamesTemplate {
    fn placeholder() -> Self {
        Self {
            games: vec![GameModel::placeholder()],
            user_id: 0,
        }
    }
}

impl Placeholder for GameTemplate {
    fn placeholder() -> Self {
        Self {
            game: GameModel::placeholder(),
            editing: false,
            user_id: 0,
        }
    }
}

openapi_template!(GameTemplate, game);
openapi_template!(AllGamesTemplate, games);

#[utoipa::path(
    get,
    path = "/games",
    tag = "Games",
    description = "Gets all the games in the exchange list.",
    responses(
        (status = OK, description = "Ok",
            content(
                (inline(AllGamesTemplate) = "text/html", example = AllGamesTemplate::render_placeholder),
                ([GameModel], example = json!([GameModel::placeholder()]))
            )
        ),
        (status = "4XX", description = "You did something wrong",
            content(
                (Error, example = Error::placeholder),
            )
        ),
        (status = "5XX", description = "We did something wrong",
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
pub async fn get_all_games(
    DatabaseConnection(mut conn, _, user): DatabaseConnection,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
) -> Result<HtmlOrJsonOnce<AllGamesTemplate>, error::Error> {
    let games = GameModel::query()
        .load(&mut conn)
        .await
        .wrap_err("Failed to get updated games list")
        .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(HtmlOrJsonOnce(
        accept,
        AllGamesTemplate {
            games,
            user_id: user.map(|u| u.id).unwrap_or_default(),
        },
    ))
}

#[derive(Deserialize, Debug)]
pub struct GetGameQuery {
    edit: Option<bool>,
}

#[utoipa::path(
    get,
    path = "/games/{game_id}",
    tag = "Games",
    description = "Gets a specific game in the exchange list.",
    responses(
        (status = OK, description = "Ok",
            content(
                (inline(GameTemplate) = "text/html", example = GameTemplate::render_placeholder),
                (GameModel, example = GameModel::placeholder)
            )
        ),
        (status = "4XX", description = "You did something wrong",
            content(
                (Error, example = Error::placeholder),
            )
        ),
        (status = "5XX", description = "We did something wrong",
            content(
                (Error, example = Error::placeholder),
            )
        ),
    ),
    security(
        ("basic_auth" = []),
        ("bearer_jwt" = []),
        ("cookie_jwt" = []),
    ),
    params(
        ("game_id" = i32, Path, description = "Game ID to retreive"),
        ("edit" = Option<bool>, Query, description = "If Accept is text/html, makes all the form fields editable if authorized")
    )
)]
#[instrument(skip(conn))]
pub async fn get_game(
    DatabaseConnection(mut conn, _, user): DatabaseConnection,
    Query(edit): Query<GetGameQuery>,
    Path(game_id): Path<i32>,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
) -> Result<HtmlOrJsonSimple<GameTemplate>, error::Error> {
    let game = GameModel::query()
        .filter(games::dsl::id.eq(game_id))
        .get_result(&mut conn)
        .await
        .wrap_err("Failed to get updated games list")
        .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

    let user_id = user.map(|u| u.id);
    Ok(HtmlOrJsonSimple(
        accept,
        GameTemplate {
            editing: edit.edit.unwrap_or_default() && user_id == Some(game.user.id),
            user_id: user_id.unwrap_or_default(),
            game,
        },
    ))
}

#[utoipa::path(
    post,
    path = "/games",
    tag = "Games",
    description = "Add a new game to the exchange list.",
    request_body(content(
        (InsertableGame, example = InsertableGame::placeholder),
        (InsertableGame = "application/x-www-form-urlencoded")
    )),
    responses(
        (status = OK, description = "Ok",
            content(
                (inline(AllGamesTemplate) = "text/html", example = AllGamesTemplate::render_placeholder),
                ([GameModel], example = json!([GameModel::placeholder()]))
            )
        ),
        (status = "4XX", description = "You did something wrong",
            content(
                (Error, example = Error::placeholder),
            )
        ),
        (status = "5XX", description = "We did something wrong",
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
pub async fn add_game(
    DatabaseConnection(mut conn, _, user): DatabaseConnection,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
    JsonOrForm(mut new_game): JsonOrForm<InsertableGame>,
) -> Result<HtmlOrJsonOnce<AllGamesTemplate>, error::Error> {
    if let Some(user) = user {
        new_game.owned_by = user.id;

        diesel::insert_into(games::table)
            .values(new_game)
            .execute(&mut conn)
            .await
            .wrap_err("Failed to insert game into database")
            .with_status_code(StatusCode::BAD_REQUEST)?;

        let games = GameModel::query()
            .load(&mut conn)
            .await
            .wrap_err("Failed to get updated games list")
            .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

        Ok(HtmlOrJsonOnce(
            accept,
            AllGamesTemplate {
                games,
                user_id: user.id,
            },
        ))
    } else {
        Err(eyre!("You aren't logged in")).with_status_code(StatusCode::UNAUTHORIZED)
    }
}

#[utoipa::path(
    put,
    path = "/games/{game_id}",
    tag = "Games",
    description = "Replace all properties of a game (full update).",
    request_body(content(
        (InsertableGame, example = InsertableGame::placeholder),
        (InsertableGame = "application/x-www-form-urlencoded")
    )),
    responses(
        (status = OK, description = "Ok",
            content(
                (inline(GameTemplate) = "text/html", example = GameTemplate::render_placeholder),
                (GameModel, example = GameModel::placeholder)
            )
        ),
        (status = "4XX", description = "You did something wrong",
            content(
                (Error, example = Error::placeholder),
            )
        ),
        (status = "5XX", description = "We did something wrong",
            content(
                (Error, example = Error::placeholder),
            )
        ),
    ),
    params(("game_id" = i32, Path, description = "Game ID to fully update")),
    security(
        ("basic_auth" = []),
        ("bearer_jwt" = []),
        ("cookie_jwt" = []),
    )
)]
#[instrument(skip(conn))]
pub async fn update_game(
    DatabaseConnection(mut conn, _, user): DatabaseConnection,
    Path(game_id): Path<i32>,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
    JsonOrForm(mut new_game): JsonOrForm<InsertableGame>,
) -> Result<HtmlOrJsonSimple<GameTemplate>, error::Error> {
    let user_id = user.map(|u| u.id).unwrap_or_default();
    new_game.owned_by = user_id;

    diesel::update(games::table)
        .filter(games::id.eq(game_id))
        .set(new_game)
        .execute(&mut conn)
        .await
        .wrap_err("Failed to update game in database")
        .with_status_code(StatusCode::BAD_REQUEST)?;

    let updated_game = GameModel::query()
        .filter(games::id.eq(game_id))
        .get_result(&mut conn)
        .await
        .wrap_err("Failed to get updated game in database")
        .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(HtmlOrJsonSimple(
        accept,
        GameTemplate {
            game: updated_game,
            editing: false,
            user_id,
        },
    ))
}

#[utoipa::path(
    patch,
    path = "/games/{game_id}",
    tag = "Games",
    description = "Update certain properties of a game (partial update).",
    request_body(content(
        (ChangesetGame, example = ChangesetGame::placeholder),
    )),
    responses(
        (status = OK, description = "Ok",
            content(
                (inline(GameTemplate) = "text/html", example = GameTemplate::render_placeholder),
                (GameModel, example = GameModel::placeholder)
            )
        ),
        (status = "4XX", description = "You did something wrong",
            content(
                (Error, example = Error::placeholder),
            )
        ),
        (status = "5XX", description = "We did something wrong",
            content(
                (Error, example = Error::placeholder),
            )
        ),
    ),
    params(("game_id" = i32, Path, description = "Game ID to partially update")),
    security(
        ("basic_auth" = []),
        ("bearer_jwt" = []),
        ("cookie_jwt" = []),
    )
)]
#[instrument(skip(conn))]
pub async fn patch_game(
    DatabaseConnection(mut conn, _, user): DatabaseConnection,
    Path(game_id): Path<i32>,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
    Json(changeset_game): Json<ChangesetGame>,
) -> Result<HtmlOrJsonSimple<GameTemplate>, error::Error> {
    diesel::update(games::table)
        .filter(games::id.eq(game_id))
        .set(changeset_game)
        .execute(&mut conn)
        .await
        .wrap_err("Failed to update game in database")
        .with_status_code(StatusCode::BAD_REQUEST)?;

    let updated_game = GameModel::query()
        .filter(games::id.eq(game_id))
        .get_result(&mut conn)
        .await
        .wrap_err("Failed to get updated game in database")
        .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(HtmlOrJsonSimple(
        accept,
        GameTemplate {
            game: updated_game,
            editing: false,
            user_id: user.map(|u| u.id).unwrap_or_default(),
        },
    ))
}

#[utoipa::path(
    delete,
    path = "/games/{game_id}",
    tag = "Games",
    description = "Remove a game from the exchange list.",
    responses(
        (status = OK, description = "Ok",
            content(
                (String = "text/html", example = ""),
                ((), example = "")
            )
        ),
        (status = "4XX", description = "You did something wrong",
            content(
                (Error, example = Error::placeholder),
            )
        ),
        (status = "5XX", description = "We did something wrong",
            content(
                (Error, example = Error::placeholder),
            )
        ),
    ),
    params(("game_id" = i32, Path, description = "Game ID to delete")),
    security(
        ("basic_auth" = []),
        ("bearer_jwt" = []),
        ("cookie_jwt" = []),
    )
)]
#[instrument(skip(conn))]
pub async fn delete_game(
    DatabaseConnection(mut conn, _, _): DatabaseConnection,
    Path(game_id): Path<i32>,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
) -> Result<(), error::Error> {
    diesel::delete(games::table)
        .filter(games::id.eq(game_id))
        .execute(&mut conn)
        .await
        .wrap_err("Failed to delete game in database")
        .with_status_code(StatusCode::BAD_REQUEST)?;

    Ok(())
}
