use axum::{
    extract::{FromRequestParts, OptionalFromRequestParts, Query},
    http::StatusCode,
    response::{Html, Redirect},
};
use axum_extra::{
    TypedHeader,
    extract::{CookieJar, cookie::Cookie},
    headers::{
        Authorization,
        authorization::{Basic, Credentials},
    },
};
use blake3::{Hash, OUT_LEN};
use color_eyre::eyre::{Context, eyre};
use diesel::{
    ExpressionMethods, HasQuery, QueryDsl,
    backend::Backend,
    deserialize::{self, FromSql, FromSqlRow},
    expression::AsExpression,
    prelude::{AsChangeset, Insertable},
    serialize::{self, Output, ToSql},
    sql_types,
};
use diesel_async::RunQueryDsl;
use sailfish::TemplateSimple;
use serde::{Deserialize, Serialize};
use tracing::instrument;
use utoipa::ToSchema;

use crate::{
    Placeholder,
    api::auth::pool::DatabaseConnection,
    error::{self, Error, WithStatusCode},
    html_or_json::HtmlOrJsonHeader,
    json_or_form::JsonOrForm,
    openapi_template,
    schema::users,
};

use pool::Pool;

#[derive(HasQuery, ToSchema, Deserialize, Serialize, Debug, Default)]
#[diesel(table_name = crate::schema::users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct User {
    pub id: i32,
    pub username: String,
}

#[derive(ToSchema, Deserialize, Serialize, Debug, Default)]
pub struct Login {
    username: String,
    password: String,
}

#[repr(transparent)]
#[derive(Debug, PartialEq, AsExpression, FromSqlRow)]
#[diesel(sql_type = sql_types::Binary)]
pub struct DieselHash(Hash);

impl<ST, DB> FromSql<ST, DB> for DieselHash
where
    DB: Backend,
    *const [u8]: FromSql<ST, DB>,
{
    #[allow(unsafe_code)] // ptr dereferencing
    fn from_sql(bytes: DB::RawValue<'_>) -> deserialize::Result<Self> {
        let slice_ptr = <*const [u8] as FromSql<ST, DB>>::from_sql(bytes)?;
        // We know that the pointer impl will never return null
        let bytes = unsafe { &*slice_ptr };
        let result: [u8; OUT_LEN] = bytes.try_into()?;
        Ok(DieselHash(result.into()))
    }
}

impl Into<Hash> for DieselHash {
    fn into(self) -> Hash {
        self.0
    }
}

impl Into<DieselHash> for Hash {
    fn into(self) -> DieselHash {
        DieselHash(self)
    }
}

impl<DB> ToSql<sql_types::Binary, DB> for DieselHash
where
    DB: Backend,
    [u8; OUT_LEN]: ToSql<sql_types::Binary, DB>,
{
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, DB>) -> serialize::Result {
        self.0.as_bytes().to_sql(out)
    }
}

#[derive(Insertable, AsChangeset, Debug, PartialEq)]
#[diesel(table_name = crate::schema::users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct InsertableDatabaseUser {
    username: String,
    #[diesel(serialize_as = DieselHash)]
    password: Hash,
}

impl Into<InsertableDatabaseUser> for Login {
    fn into(self) -> InsertableDatabaseUser {
        let mut hash = blake3::Hasher::new();
        hash.update(self.username.as_bytes());
        hash.update(self.password.as_bytes());
        InsertableDatabaseUser {
            username: self.username,
            password: hash.finalize(),
        }
    }
}

impl Into<InsertableDatabaseUser> for Basic {
    fn into(self) -> InsertableDatabaseUser {
        let mut hash = blake3::Hasher::new();
        hash.update(self.username().as_bytes());
        hash.update(self.password().as_bytes());
        InsertableDatabaseUser {
            username: self.username().to_owned(),
            password: hash.finalize(),
        }
    }
}

#[derive(HasQuery, Debug, PartialEq)]
#[diesel(table_name = crate::schema::users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct DatabaseUser {
    id: i32,
    username: String,
    #[diesel(deserialize_as = DieselHash)]
    password: Hash,
}

impl Placeholder for User {
    fn placeholder() -> Self {
        Self {
            id: 1,
            username: String::from("johndoe"),
        }
    }
}

impl Placeholder for Login {
    fn placeholder() -> Self {
        Self {
            username: String::from("johndoe"),
            password: String::from("verySecurePassword1234"),
        }
    }
}

impl FromRequestParts<Pool> for User {
    type Rejection = error::Error;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &Pool,
    ) -> Result<Self, Self::Rejection> {
        <User as OptionalFromRequestParts<Pool>>::from_request_parts(parts, state)
            .await?
            .ok_or_else(|| eyre!("Your user wasn't found"))
            .with_status_code(StatusCode::UNAUTHORIZED)
    }
}

pub mod pool {
    use axum::{
        extract::{FromRequestParts, OptionalFromRequestParts},
        http::{HeaderValue, StatusCode},
    };
    use axum_extra::{
        TypedHeader,
        extract::CookieJar,
        headers::{
            Authorization,
            authorization::{Basic, Bearer, Credentials},
        },
    };
    use color_eyre::eyre::{Context, eyre};
    use diesel::{ExpressionMethods, HasQuery, QueryDsl};
    use diesel_async::{
        AsyncPgConnection, RunQueryDsl,
        pooled_connection::bb8::{self, RunError},
    };
    use tracing::instrument;

    use crate::{
        api::auth::{DatabaseUser, InsertableDatabaseUser, User},
        error::{self, Actions, WithStatusCode},
        schema::users,
    };

    #[derive(Clone)]
    pub struct Pool(bb8::Pool<AsyncPgConnection>);

    impl Pool {
        pub fn new(pool: bb8::Pool<AsyncPgConnection>) -> Self {
            Self(pool)
        }

        fn get(
            &self,
        ) -> impl Future<Output = Result<bb8::PooledConnection<'_, AsyncPgConnection>, RunError>>
        {
            self.0.get()
        }

        fn get_owned(
            &self,
        ) -> impl Future<Output = Result<bb8::PooledConnection<'static, AsyncPgConnection>, RunError>>
        {
            self.0.get_owned()
        }
    }

    pub struct DatabaseConnection(
        pub bb8::PooledConnection<'static, AsyncPgConnection>,
        pub CookieJar,
        pub Option<User>,
    );

    impl OptionalFromRequestParts<Pool> for User {
        type Rejection = error::Error;

        #[instrument(skip_all)]
        async fn from_request_parts(
            parts: &mut axum::http::request::Parts,
            pool: &Pool,
        ) -> Result<Option<Self>, Self::Rejection> {
            let cookie_jar = CookieJar::from_request_parts(parts, pool)
                .await
                .wrap_err("Failed to retreive cookies from header")
                .with_status_code_and_actions(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Actions::sign_out(),
                )?;

            if let Some(TypedHeader(Authorization(basic_auth))) = <TypedHeader<Authorization<Basic>> as OptionalFromRequestParts<
            Pool,
            >>::from_request_parts(parts, pool)
            .await
            .wrap_err("Failed to parse basic auth header")
            .with_status_code_and_actions(StatusCode::BAD_REQUEST, Actions::sign_out())?
            .or_else(|| cookie_jar.get("sessionid").and_then(|sessionid| Some(TypedHeader(Authorization(Basic::decode(&HeaderValue::from_str(sessionid.value()).ok()?)?)))))
            {
                let mut conn = pool
                    .get()
                    .await
                    .wrap_err("Failed to get connection to database")
                    .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

                let user = DatabaseUser::query().filter(users::username.eq(basic_auth.username())).get_result(&mut conn).await.wrap_err("Failed to get user from database").with_status_code_and_actions(StatusCode::INTERNAL_SERVER_ERROR, Actions::sign_out())?;     

                let login_attempt: InsertableDatabaseUser = basic_auth.into();

                if login_attempt.password != user.password {
                    return Err(eyre!("Passwords didn't match")).with_status_code_and_actions(StatusCode::UNAUTHORIZED, Actions::sign_out())
                }

                return Ok(Some(User {
                    id: user.id,
                    username: user.username
                }))
            }

            if let Some(TypedHeader(Authorization(bearer_auth))) = <TypedHeader<
                Authorization<Bearer>,
            > as OptionalFromRequestParts<Pool>>::from_request_parts(
                parts, pool
            )
            .await
            .wrap_err("Failed to parse bearer auth header")
            .with_status_code_and_actions(StatusCode::BAD_REQUEST, Actions::sign_out())?
            {
            }

            return Ok(None);
        }
    }

    impl FromRequestParts<Pool> for DatabaseConnection {
        type Rejection = error::Error;

        #[instrument(skip_all)]
        async fn from_request_parts(
            parts: &mut axum::http::request::Parts,
            pool: &Pool,
        ) -> Result<Self, Self::Rejection> {
            let user =
                <User as OptionalFromRequestParts<Pool>>::from_request_parts(parts, pool).await?;

            let cookie_jar = CookieJar::from_request_parts(parts, pool)
                .await
                .wrap_err("Failed to retreive cookies from header")
                .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

            let mut conn = pool
                .get_owned()
                .await
                .wrap_err("Failed to get connection to database")
                .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

            diesel::sql_query(r#"SELECT set_config('app.current_user_id', $1::text, false)"#)
                .bind::<diesel::sql_types::Integer, _>(
                    user.as_ref().map(|u| u.id).unwrap_or_default(),
                )
                .execute(&mut conn)
                .await
                .wrap_err("Failed to set user id on connection")
                .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

            Ok(Self(conn, cookie_jar, user))
        }
    }
}

#[utoipa::path(
    post,
    path = "/auth/signup",
    tag = "Users",
    description = "Create a new account",
    request_body(content(
        (Login, example = Login::placeholder),
        (Login = "application/x-www-form-urlencoded")
    )),
    responses(
        (status = OK, description = "Ok",
            headers(
                ("Set-Cookie" = String)
            ),
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
)]
#[instrument(skip(conn))]
pub async fn signup(
    DatabaseConnection(mut conn, jar, _): DatabaseConnection,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
    JsonOrForm(new_user): JsonOrForm<Login>,
) -> Result<(CookieJar, Redirect), error::Error> {
    let encoded = Authorization::basic(&new_user.username, &new_user.password);
    let db_user: InsertableDatabaseUser = new_user.into();

    diesel::insert_into(users::table)
        .values(db_user)
        .execute(&mut conn)
        .await
        .wrap_err("Failed to insert user into database")
        .with_status_code(StatusCode::BAD_REQUEST)?;

    let header_value = encoded.0.encode();
    let mut cookie = Cookie::new(
        "sessionid",
        header_value
            .to_str()
            .wrap_err("Failed to encode sessionid")
            .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?
            .to_owned(),
    );
    cookie.set_path("/");
    Ok((jar.add(cookie), Redirect::to("/")))
}

#[utoipa::path(
    post,
    path = "/auth/login",
    tag = "Users",
    description = "Login to your account",
    request_body(content(
        (Login, example = Login::placeholder),
        (Login = "application/x-www-form-urlencoded")
    )),
    responses(
        (status = OK, description = "Ok",
            headers(
                ("Set-Cookie" = String)
            ),
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
)]
#[instrument(skip(conn))]
pub async fn login(
    DatabaseConnection(mut conn, jar, _): DatabaseConnection,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
    JsonOrForm(new_user): JsonOrForm<Login>,
) -> Result<(CookieJar, Redirect), error::Error> {
    let encoded = Authorization::basic(&new_user.username, &new_user.password);
    let db_user: InsertableDatabaseUser = new_user.into();

    let user = DatabaseUser::query()
        .filter(users::username.eq(db_user.username))
        .get_result(&mut conn)
        .await
        .wrap_err("Failed to get user from database")
        .with_status_code(StatusCode::BAD_REQUEST)?;

    if user.password == db_user.password {
        let header_value = encoded.0.encode();
        let mut cookie = Cookie::new(
            "sessionid",
            header_value
                .to_str()
                .wrap_err("Failed to encode sessionid")
                .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?
                .to_owned(),
        );
        cookie.set_path("/");
        Ok((jar.add(cookie), Redirect::to("/")))
    } else {
        Err(eyre!("Invalid username or password")).with_status_code(StatusCode::UNAUTHORIZED)
    }
}

#[derive(TemplateSimple)]
#[template(path = "login.stpl")]
#[template(rm_whitespace = true, rm_newline = true)]
pub struct LoginTemplate {
    user: Option<User>,
    editing: bool,
}

impl Placeholder for LoginTemplate {
    fn placeholder() -> Self {
        Self {
            user: None,
            editing: false,
        }
    }
}

openapi_template!(LoginTemplate, user);

#[derive(Deserialize, Debug)]
pub struct EditQuery {
    edit: Option<bool>,
}

#[utoipa::path(
    get,
    path = "/auth/login",
    tag = "Users",
    description = "Get login form as HTML",
    responses(
        (status = OK, description = "Ok",
            content(
                (inline(LoginTemplate) = "text/html", example = LoginTemplate::render_placeholder),
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
    params(
        ("edit" = Option<bool>, Query, description = "If logged in, returns a form for your credentials that is editable")
    )
)]
#[instrument]
pub async fn get_login(
    user: Option<User>,
    Query(edit): Query<EditQuery>,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
) -> Result<Html<String>, error::Error> {
    Ok(Html(
        LoginTemplate {
            user,
            editing: edit.edit.unwrap_or_default(),
        }
        .render_once()
        .wrap_err("Failed to render login template")
        .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?,
    ))
}

#[utoipa::path(
    post,
    path = "/auth/patchlogin",
    tag = "Users",
    description = "Edit login information",
    request_body(content(
        (Login, example = Login::placeholder),
        (Login = "application/x-www-form-urlencoded")
    )),
    responses(
        (status = OK, description = "Ok",
            content(
                (inline(LoginTemplate) = "text/html", example = LoginTemplate::render_placeholder),
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
)]
#[instrument(skip(conn))]
pub async fn patch_login(
    DatabaseConnection(mut conn, jar, user): DatabaseConnection,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
    JsonOrForm(changeset_user): JsonOrForm<Login>,
) -> Result<(CookieJar, Redirect), error::Error> {
    let encoded = Authorization::basic(&changeset_user.username, &changeset_user.password);
    let user_id = user.map(|u| u.id).unwrap_or_default();
    let db_user: InsertableDatabaseUser = changeset_user.into();

    diesel::update(users::table)
        .filter(users::id.eq(user_id))
        .set(db_user)
        .execute(&mut conn)
        .await
        .wrap_err("Failed to update user in database")
        .with_status_code(StatusCode::BAD_REQUEST)?;

    let header_value = encoded.0.encode();
    let mut cookie = Cookie::new(
        "sessionid",
        header_value
            .to_str()
            .wrap_err("Failed to encode sessionid")
            .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?
            .to_owned(),
    );
    cookie.set_path("/");

    Ok((jar.add(cookie), Redirect::to("/")))
}

#[utoipa::path(
    get,
    path = "/auth/logout",
    tag = "Users",
    description = "Logout of account",
    responses(
        (status = OK, description = "Ok",
            headers(
                ("Set-Cookie" = String)
            ),
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
)]
#[instrument]
pub async fn logout(cookie_jar: CookieJar) -> (CookieJar, Redirect) {
    let mut cookie = Cookie::from("sessionid");
    cookie.set_path("/");
    (cookie_jar.remove(cookie), Redirect::to("/"))
}
