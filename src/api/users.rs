use axum::{Json, extract::Path, http::StatusCode};
use color_eyre::eyre::{Context, OptionExt};
use diesel::{ExpressionMethods, HasQuery, OptionalExtension, query_dsl::methods::FilterDsl};
use diesel_async::RunQueryDsl;
use serde::{Deserialize, Serialize, Serializer};
use tracing::instrument;
use utoipa::ToSchema;

use crate::{
    Placeholder,
    api::auth::pool::DatabaseConnection,
    error::{self, Error, WithStatusCode},
    schema::users,
};

#[derive(HasQuery, ToSchema, Deserialize, Serialize, Debug, Default)]
#[diesel(table_name = crate::schema::users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct User {
    pub id: i32,
    pub username: String,
}

impl Placeholder for User {
    fn placeholder() -> Self {
        Self {
            id: 1,
            username: String::from("johndoe"),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct UserClaims {
    pub exp: u64,
    #[serde(flatten)]
    pub user: User,
}

pub fn serialize_user_id<S>(user: &User, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&format!("/users/{}", user.id))
}

#[utoipa::path(
    get,
    path = "/user/{user_id}",
    tag = "Users",
    description = "Get a user",
    responses(
        (status = OK, description = "Ok",
            content(
                (User, example = User::placeholder),
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
        ("user_id" = i32, Path, description = "User ID to retreive"),
    )
)]
#[instrument(skip(conn))]
pub async fn get_user(
    Path(user_id): Path<i32>,
    DatabaseConnection(mut conn, _, _): DatabaseConnection,
) -> Result<Json<User>, error::Error> {
    let user = User::query()
        .filter(users::id.eq(user_id))
        .first(&mut conn)
        .await
        .optional()
        .wrap_err("Failed to get user from database")
        .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or_eyre("Couldn't find user in database")
        .with_status_code(StatusCode::NOT_FOUND)?;

    Ok(Json(user))
}
