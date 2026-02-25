use std::fmt::Display;

use axum::{
    Json,
    extract::{Path, Query},
    http::StatusCode,
};
use axum_extra::TypedHeader;
use color_eyre::eyre::{Context, OptionExt, eyre};
use diesel::{ExpressionMethods, HasQuery, QueryDsl, prelude::*};
use diesel_async::RunQueryDsl;
use diesel_derive_enum::DbEnum;
use pgvector::VectorExpressionMethods;
use sailfish::{TemplateOnce, TemplateSimple};
use serde::{Deserialize, Serialize};
use tracing::instrument;
use utoipa::ToSchema;

use crate::{
    Placeholder, SearchQuery,
    api::{
        auth::pool::DatabaseConnection,
        users::{User, serialize_user_id},
    },
    conditional_query,
    embeddings::EmbeddingRetreiver,
    error::{self, Error, WithStatusCode},
    html_or_json::{HtmlOrJsonHeader, HtmlOrJsonOnce, HtmlOrJsonSimple},
    htmx::{HxQuery, HxRequest},
    json_or_form::JsonOrForm,
    openapi_template_render, openapi_template_serialize, openapi_template_utoipa,
    schema::{games, sql_types, users},
};

fn default_pgvector() -> pgvector::Vector {
    pgvector::Vector::from(Vec::new())
}

#[derive(HasQuery, Insertable, AsChangeset, ToSchema, Deserialize, Serialize, Debug)]
#[diesel(table_name = crate::schema::games)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct InsertableGame {
    pub name: String,
    #[serde(skip)]
    pub owned_by: i32,
    #[diesel(treat_none_as_null = true)]
    pub publisher: Option<String>,
    #[diesel(treat_none_as_null = true)]
    #[schema(minimum = 0, maximum = 65535)]
    pub year: Option<i16>,
    #[diesel(treat_none_as_null = true)]
    pub platform: Option<String>,
    #[diesel(treat_none_as_null = true)]
    pub condition: Option<Condition>,
    #[serde(skip, default = "default_pgvector")]
    pub embedding: pgvector::Vector,
}

impl Display for InsertableGame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "A ")?;
        if let Some(condition) = self.condition {
            write!(f, "{} condition ", condition)?;
        }
        write!(f, "copy of the game {}, ", self.name)?;
        if self.publisher.is_some() || self.year.is_some() {
            write!(f, "published ")?;
            if let Some(publisher) = self.publisher.as_ref() {
                write!(f, "by {} ", publisher)?;
            }
            if let Some(year) = self.year.as_ref() {
                write!(f, "in {} ", year)?;
            }
        }
        if let Some(platform) = self.platform.as_ref() {
            write!(f, "for the {} platform.", platform)?;
        }
        Ok(())
    }
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
    #[serde(skip)]
    embedding: Option<pgvector::Vector>,
}

#[derive(HasQuery, ToSchema, Serialize, Debug, Default)]
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
    #[schema(value_type = String)]
    #[serde(serialize_with = "serialize_user_id")]
    pub user: User,
}

#[derive(Clone, Copy, DbEnum, ToSchema, Deserialize, Serialize, Debug, PartialEq)]
#[db_enum(existing_type_path = "sql_types::Condition")]
pub enum Condition {
    Mint,
    Good,
    Fair,
    Poor,
}

impl Display for Condition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Mint => write!(f, "mint"),
            Self::Good => write!(f, "good"),
            Self::Fair => write!(f, "fair"),
            Self::Poor => write!(f, "poor"),
        }
    }
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
            embedding: pgvector::Vector::from(Vec::new()),
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
            embedding: Some(pgvector::Vector::from(Vec::new())),
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

#[derive(TemplateOnce, Default)]
#[template(path = "games/all_games.stpl")]
#[template(rm_whitespace = true, rm_newline = true)]
pub struct AllGamesTemplate {
    games: Vec<GameModel>,
    search_query: String,
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
            search_query: String::new(),
            user_id: 1,
        }
    }
}

impl Placeholder for GameTemplate {
    fn placeholder() -> Self {
        Self {
            game: GameModel::placeholder(),
            editing: false,
            user_id: 1,
        }
    }
}

openapi_template_utoipa!(GameTemplate);
openapi_template_serialize!(GameTemplate, game);
openapi_template_render!(GameTemplate, render_placeholder, placeholder);
openapi_template_utoipa!(AllGamesTemplate);
openapi_template_serialize!(AllGamesTemplate, games);
openapi_template_render!(AllGamesTemplate, render_placeholder, placeholder);
openapi_template_render!(AllGamesTemplate, render_default, default);

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
        (status = UNAUTHORIZED, description = "You aren't logged in",
            content(
                (inline(AllGamesTemplate) = "text/html", example = AllGamesTemplate::render_default),
                ([GameModel], example = json!([]))
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
    params(
        ("q" = String, Query, description = "Search query for games"),
        ("uid" = i64, Query, description = "The user ID to grab games from")
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
    embedding_retreiver: EmbeddingRetreiver,
    Query(search): Query<SearchQuery>,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
    TypedHeader(hx_request): TypedHeader<HxRequest>,
) -> Result<(StatusCode, HtmlOrJsonOnce<AllGamesTemplate>), error::Error> {
    let games = conditional_query!(
        search.q.is_empty(),
        query_w_search,
        GameModel::query(),
        {
            let embeddings = embedding_retreiver
                .get_embeddings(&format!("task: search result | query: {}", search.q))
                .await
                .wrap_err("Failed to create embeddings from search query")
                .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

            GameModel::query().order_by(games::embedding.max_inner_product(embeddings))
        },
        conditional_query!(
            search.uid == 0,
            query_w_uid,
            query_w_search,
            query_w_search.filter(games::owned_by.eq(search.uid)),
            query_w_uid
                .load(&mut conn)
                .await
                .wrap_err("Failed to get updated games list")
                .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?
        )
    );

    // GameModel::query()
    //     .filter(games::tsembedding.matches(plainto_tsquery(&search.q)))
    //     .order_by(ts_rank_cd(games::tsembedding, plainto_tsquery(&search.q)).desc())
    //     .load(&mut conn)
    //     .await
    //     .wrap_err("Failed to get updated games list")
    //     .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?

    // use diesel::sql_types::*;
    // use pgvector::sql_types::*;

    // GameModel::query()
    //     .order_by(1 - games::embedding.max_inner_product(embeddings))
    //     .load(&mut conn)
    //     .await
    //     .wrap_err("Failed to get updated games list")
    //     .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?

    // diesel::sql_query("SELECT * FROM hybrid_search($1, $2, $3)")
    //     .bind::<Text, _>(search.q) // query_text
    //     .bind::<Vector, _>(embeddings) // query_embedding
    //     .bind::<Integer, _>(30) // match_count
    //     .load::<GameModel>(&mut conn)
    //     .await
    //     .wrap_err("Failed to get updated games list")
    //     .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?

    Ok((
        if user.is_some() {
            StatusCode::OK
        } else {
            StatusCode::UNAUTHORIZED
        },
        HtmlOrJsonOnce(
            accept,
            hx_request,
            AllGamesTemplate {
                games,
                search_query: search.q,
                user_id: user.map(|u| u.id).unwrap_or_default(),
            },
        ),
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
    TypedHeader(hx_request): TypedHeader<HxRequest>,
) -> Result<HtmlOrJsonSimple<GameTemplate>, error::Error> {
    let game = GameModel::query()
        .filter(games::dsl::id.eq(game_id))
        .first(&mut conn)
        .await
        .optional()
        .wrap_err("Failed to get updated games list")
        .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or_eyre("Couldn't find that game")
        .with_status_code(StatusCode::NOT_FOUND)?;

    let user_id = user.map(|u| u.id);
    Ok(HtmlOrJsonSimple(
        accept,
        hx_request,
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
pub async fn add_game(
    DatabaseConnection(mut conn, jar, user): DatabaseConnection,
    embedding_retreiver: EmbeddingRetreiver,
    search: Option<HxQuery<SearchQuery>>,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
    TypedHeader(hx_request): TypedHeader<HxRequest>,
    JsonOrForm(mut new_game): JsonOrForm<InsertableGame>,
) -> Result<(StatusCode, HtmlOrJsonOnce<AllGamesTemplate>), error::Error> {
    if let Some(user) = user {
        new_game.owned_by = user.id;

        new_game.embedding = embedding_retreiver
            .get_embeddings(&format!("title: {} | text: {}", new_game.name, new_game))
            .await
            .wrap_err("Failed to create embeddings for new games")
            .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

        diesel::insert_into(games::table)
            .values(new_game)
            .execute(&mut conn)
            .await
            .wrap_err("Failed to insert game into database")
            .with_status_code(StatusCode::UNPROCESSABLE_ENTITY)?;

        get_all_games(
            DatabaseConnection(conn, jar, Some(user)),
            embedding_retreiver,
            Query(search.map(|s| s.0).unwrap_or_default()),
            TypedHeader(accept),
            TypedHeader(hx_request),
        )
        .await
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
    embedding_retreiver: EmbeddingRetreiver,
    Path(game_id): Path<i32>,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
    TypedHeader(hx_request): TypedHeader<HxRequest>,
    JsonOrForm(mut new_game): JsonOrForm<InsertableGame>,
) -> Result<HtmlOrJsonSimple<GameTemplate>, error::Error> {
    let user_id = user.map(|u| u.id).unwrap_or_default();
    new_game.owned_by = user_id;

    new_game.embedding = embedding_retreiver
        .get_embeddings(&format!("title: {} | text: {}", new_game.name, new_game))
        .await
        .wrap_err("Failed to create embeddings for new games")
        .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

    diesel::update(games::table)
        .filter(games::id.eq(game_id))
        .set(new_game)
        .execute(&mut conn)
        .await
        .wrap_err("Failed to update game in database")
        .with_status_code(StatusCode::UNPROCESSABLE_ENTITY)?;

    let updated_game = GameModel::query()
        .filter(games::id.eq(game_id))
        .first(&mut conn)
        .await
        .wrap_err("Failed to get updated game in database")
        .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(HtmlOrJsonSimple(
        accept,
        hx_request,
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
    TypedHeader(hx_request): TypedHeader<HxRequest>,
    Json(changeset_game): Json<ChangesetGame>,
) -> Result<HtmlOrJsonSimple<GameTemplate>, error::Error> {
    diesel::update(games::table)
        .filter(games::id.eq(game_id))
        .set(changeset_game)
        .execute(&mut conn)
        .await
        .wrap_err("Failed to update game in database")
        .with_status_code(StatusCode::UNPROCESSABLE_ENTITY)?;

    let updated_game = GameModel::query()
        .filter(games::id.eq(game_id))
        .first(&mut conn)
        .await
        .wrap_err("Failed to get updated game in database")
        .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(HtmlOrJsonSimple(
        accept,
        hx_request,
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
        .with_status_code(StatusCode::UNPROCESSABLE_ENTITY)?;

    Ok(())
}
