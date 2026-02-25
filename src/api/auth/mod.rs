use std::{
    borrow::Cow, convert::Infallible, num::NonZeroU32, ops::Deref, sync::OnceLock, time::Duration,
};

use aws_lc_rs::{digest, pbkdf2};
use axum::{
    extract::{FromRequestParts, OptionalFromRequestParts},
    http::StatusCode,
    response::Redirect,
};
use axum_extra::{
    TypedHeader,
    either::Either,
    extract::{
        CookieJar,
        cookie::{Cookie, SameSite},
    },
};
use color_eyre::eyre::{Context, OptionExt, eyre};
use diesel::{
    ExpressionMethods, HasQuery, OptionalExtension, QueryDsl, SelectableHelper,
    backend::Backend,
    deserialize::{self, FromSql, FromSqlRow},
    expression::AsExpression,
    prelude::{AsChangeset, Insertable},
    serialize::{self, Output, ToSql},
    sql_types,
};
use diesel_async::RunQueryDsl;
use opentelemetry::global;
use rdkafka::{message::OwnedHeaders, producer::FutureRecord};
use serde::{Deserialize, Serialize};
use tracing::instrument;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{RefOr, Schema},
};

pub mod pool;

static PBKDF2_ALG: pbkdf2::Algorithm = pbkdf2::PBKDF2_HMAC_SHA256;
const CREDENTIAL_LEN: usize = digest::SHA256_OUTPUT_LEN;
const SALT_LEN: usize = 16;
const PBKDF2_ITERATIONS: NonZeroU32 = NonZeroU32::new(310_000).unwrap();
pub type Credential = [u8; CREDENTIAL_LEN];
pub type Salt = [u8; SALT_LEN];
pub static JWT_HEADER: OnceLock<jsonwebtoken::Header> = OnceLock::new();

use crate::{
    ApiState, KafkaState, Placeholder,
    api::{
        auth::pool::DatabaseConnection,
        users::{User, UserClaims},
    },
    error::{self, Error, WithStatusCode},
    html_or_json::HtmlOrJsonHeader,
    htmx::{HxLocation, HxRefresh, HxRequest},
    json_or_form::JsonOrForm,
    kafka::KafkaMessage,
    schema::users,
    telemetry::KafkaOwnedHeaderCarrier,
};

#[repr(transparent)]
#[derive(Debug, PartialEq, AsExpression, FromSqlRow)]
#[diesel(sql_type = sql_types::Binary)]
pub struct DieselByteA<const N: usize>([u8; N]);

impl<ST, DB, const N: usize> FromSql<ST, DB> for DieselByteA<N>
where
    DB: Backend,
    *const [u8]: FromSql<ST, DB>,
{
    #[allow(unsafe_code)] // ptr dereferencing
    fn from_sql(bytes: DB::RawValue<'_>) -> deserialize::Result<Self> {
        let slice_ptr = <*const [u8] as FromSql<ST, DB>>::from_sql(bytes)?;
        // We know that the pointer impl will never return null
        let bytes = unsafe { &*slice_ptr };
        let result: [u8; N] = bytes.try_into()?;
        Ok(DieselByteA(result))
    }
}

impl<const N: usize> Into<[u8; N]> for DieselByteA<N> {
    fn into(self) -> [u8; N] {
        self.0
    }
}

impl<const N: usize> Into<DieselByteA<N>> for [u8; N] {
    fn into(self) -> DieselByteA<N> {
        DieselByteA(self)
    }
}

impl<DB, const N: usize> ToSql<sql_types::Binary, DB> for DieselByteA<N>
where
    DB: Backend,
    [u8]: ToSql<sql_types::Binary, DB>,
{
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, DB>) -> serialize::Result {
        self.0.as_slice().to_sql(out)
    }
}

impl<const N: usize> Placeholder for DieselByteA<N> {
    fn placeholder() -> Self {
        Self([0; N])
    }
}

#[derive(ToSchema, Deserialize, Serialize, Debug, Default)]
pub struct Login {
    username: String,
    password: String,
}

#[derive(Insertable, AsChangeset, Debug, PartialEq)]
#[diesel(table_name = crate::schema::users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct DatabaseUserPassword {
    #[diesel(skip_insertion, skip_update)]
    password_str: String,
    pbkdf2_iterations: i32,
    salt: DieselByteA<SALT_LEN>,
    password: DieselByteA<CREDENTIAL_LEN>,
}

impl Placeholder for DatabaseUserPassword {
    fn placeholder() -> Self {
        Self {
            password_str: String::from("example"),
            pbkdf2_iterations: PBKDF2_ITERATIONS.get() as i32,
            salt: DieselByteA::placeholder(),
            password: DieselByteA::placeholder(),
        }
    }
}

#[derive(ToSchema, Deserialize, Serialize)]
pub struct DatabaseUserPasswordRaw<'a> {
    password: Cow<'a, str>,
}

impl PartialSchema for DatabaseUserPassword {
    fn schema() -> RefOr<Schema> {
        DatabaseUserPasswordRaw::schema()
    }
}

impl ToSchema for DatabaseUserPassword {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("DatabaseUserPassword")
    }

    fn schemas(
        schemas: &mut Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) {
        DatabaseUserPasswordRaw::schemas(schemas);
    }
}

impl<'de> Deserialize<'de> for DatabaseUserPassword {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let password_raw = DatabaseUserPasswordRaw::deserialize(deserializer)?;
        let mut salt: Salt = [0; 16];
        aws_lc_rs::rand::fill(&mut salt).map_err(|e| D::Error::custom(e))?;
        let mut password: Credential = [0; CREDENTIAL_LEN];
        pbkdf2::derive(
            PBKDF2_ALG,
            PBKDF2_ITERATIONS,
            &salt,
            password_raw.password.as_bytes(),
            &mut password,
        );

        Ok(Self {
            password_str: password_raw.password.into_owned(),
            pbkdf2_iterations: PBKDF2_ITERATIONS.get() as i32,
            password: DieselByteA(password),
            salt: DieselByteA(salt),
        })
    }
}

impl Serialize for DatabaseUserPassword {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        DatabaseUserPasswordRaw {
            password: Cow::Borrowed(&self.password_str),
        }
        .serialize(serializer)
    }
}

#[derive(Insertable, AsChangeset, ToSchema, Deserialize, Serialize, Debug, PartialEq)]
#[diesel(table_name = crate::schema::users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct InsertableDatabaseUser {
    username: String,
    mailing_address_1: Option<String>,
    mailing_address_2: Option<String>,
    city: Option<String>,
    state: Option<String>,
    zip: Option<String>,
    #[serde(flatten)]
    #[diesel(embed)]
    password: DatabaseUserPassword,
}

impl Placeholder for InsertableDatabaseUser {
    fn placeholder() -> Self {
        Self {
            username: String::from("johndoe"),
            mailing_address_1: Some(String::from("1234 Road St")),
            mailing_address_2: None,
            city: Some(String::from("Los Angeles")),
            state: Some(String::from("CA")),
            zip: Some(String::from("12345")),
            password: DatabaseUserPassword::placeholder(),
        }
    }
}

#[derive(HasQuery, Debug, PartialEq)]
#[diesel(table_name = crate::schema::users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct DatabaseUser {
    id: i32,
    username: String,
    pbkdf2_iterations: i32,
    #[diesel(deserialize_as = DieselByteA<SALT_LEN>)]
    salt: Salt,
    #[diesel(deserialize_as = DieselByteA<CREDENTIAL_LEN>)]
    password: Credential,
}

impl Placeholder for Login {
    fn placeholder() -> Self {
        Self {
            username: String::from("johndoe"),
            password: String::from("verySecurePassword1234"),
        }
    }
}

impl Into<User> for DatabaseUser {
    fn into(self) -> User {
        User {
            id: self.id,
            username: self.username,
        }
    }
}

impl OptionalFromRequestParts<ApiState> for User {
    type Rejection = error::Error;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &ApiState,
    ) -> Result<Option<Self>, Self::Rejection> {
        <DatabaseConnection as FromRequestParts<ApiState>>::from_request_parts(parts, state)
            .await
            .map(|c| c.2)
    }
}

pub struct EncodingKeyExtractor(jsonwebtoken::EncodingKey);

impl OptionalFromRequestParts<ApiState> for EncodingKeyExtractor {
    type Rejection = Infallible;

    async fn from_request_parts(
        _parts: &mut axum::http::request::Parts,
        state: &ApiState,
    ) -> Result<Option<Self>, Self::Rejection> {
        Ok(state.encoding_key.clone().map(EncodingKeyExtractor))
    }
}

macro_rules! session_id_cookie_name {
    ($session_id_num:expr) => {
        format!("sessionid.{}", $session_id_num)
    };
}

fn session_id_cookie<'a>(session_id_num: usize, value: impl Into<Cow<'a, str>>) -> Cookie<'a> {
    let mut cookie = Cookie::new(session_id_cookie_name!(session_id_num), value);
    cookie.set_path("/");
    cookie.set_secure(true);
    cookie.set_same_site(SameSite::Strict);
    cookie.set_http_only(true);
    cookie
}

fn re_encode_cookie_jwt(encoded_jwt: String, mut jar: CookieJar) -> CookieJar {
    let mut session_id_num = 0;
    for chunk in encoded_jwt.as_bytes().chunks(3180) {
        jar = jar.add(session_id_cookie(
            session_id_num,
            std::str::from_utf8(chunk).unwrap().to_owned(),
        ));
        session_id_num += 1;
    }
    while jar.get(&session_id_cookie_name!(session_id_num)).is_some() {
        jar = jar.remove(session_id_cookie(session_id_num, ""));
        session_id_num += 1;
    }

    jar
}

#[utoipa::path(
    post,
    path = "/auth/signup",
    tag = "Users",
    description = "Create a new account",
    request_body(content(
        (InsertableDatabaseUser, example = InsertableDatabaseUser::placeholder),
        (InsertableDatabaseUser = "application/x-www-form-urlencoded")
    )),
    responses(
        (status = OK, description = "Ok",
            headers(
                ("Set-Cookie" = String)
            ),
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
)]
#[instrument(skip(conn, encoding_key))]
pub async fn signup(
    DatabaseConnection(mut conn, jar, _): DatabaseConnection,
    encoding_key: Option<EncodingKeyExtractor>,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
    TypedHeader(hx_request): TypedHeader<HxRequest>,
    JsonOrForm(new_user): JsonOrForm<InsertableDatabaseUser>,
) -> Result<(CookieJar, Either<TypedHeader<HxLocation>, Redirect>), error::Error> {
    let Some(EncodingKeyExtractor(encoding_key)) = encoding_key else {
        return Err(eyre!("This service has no encoding key configured"))
            .with_status_code(StatusCode::NOT_FOUND)?;
    };

    let user = diesel::insert_into(users::table)
        .values(new_user)
        .returning(User::as_returning())
        .get_result(&mut conn)
        .await
        .wrap_err("Failed to insert user into database")
        .with_status_code(StatusCode::UNPROCESSABLE_ENTITY)?;

    let encoded = jsonwebtoken::encode(
        JWT_HEADER
            .get()
            .ok_or_eyre("JWT_HEADER was not set")
            .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?,
        &UserClaims {
            exp: jsonwebtoken::get_current_timestamp() + 3600,
            user,
        },
        &encoding_key,
    )
    .wrap_err("Failed to encode JWT key")
    .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((
        re_encode_cookie_jwt(encoded, jar),
        if hx_request.0 {
            Either::E1(TypedHeader(HxLocation::new("/games")))
        } else {
            Either::E2(Redirect::to("/games"))
        },
    ))
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
)]
#[instrument(skip(conn, encoding_key))]
pub async fn login(
    DatabaseConnection(mut conn, jar, _): DatabaseConnection,
    encoding_key: Option<EncodingKeyExtractor>,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
    TypedHeader(hx_request): TypedHeader<HxRequest>,
    JsonOrForm(new_user): JsonOrForm<Login>,
) -> Result<(CookieJar, Either<TypedHeader<HxLocation>, Redirect>), error::Error> {
    let Some(EncodingKeyExtractor(encoding_key)) = encoding_key else {
        return Err(eyre!("This service has no encoding key configured"))
            .with_status_code(StatusCode::NOT_FOUND)?;
    };

    let user = DatabaseUser::query()
        .filter(users::username.eq(new_user.username))
        .get_result(&mut conn)
        .await
        .optional()
        .wrap_err("Failed to get user from database")
        .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or_eyre("Couldn't find user in database")
        .with_status_code(StatusCode::UNAUTHORIZED)?;

    if pbkdf2::verify(
        PBKDF2_ALG,
        NonZeroU32::new(user.pbkdf2_iterations as u32)
            .ok_or_eyre("Invalid number of PBKDF2 iterations for user")
            .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?,
        &user.salt,
        new_user.password.as_bytes(),
        &user.password,
    )
    .is_ok()
    {
        let encoded = jsonwebtoken::encode(
            JWT_HEADER
                .get()
                .ok_or_eyre("JWT_HEADER was not set")
                .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?,
            &UserClaims {
                exp: jsonwebtoken::get_current_timestamp() + 3600,
                user: user.into(),
            },
            &encoding_key,
        )
        .wrap_err("Failed to encode JWT key")
        .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

        Ok((
            re_encode_cookie_jwt(encoded, jar),
            if hx_request.0 {
                Either::E1(TypedHeader(HxLocation::new("/games")))
            } else {
                Either::E2(Redirect::to("/games"))
            },
        ))
    } else {
        Err(eyre!("Invalid username or password")).with_status_code(StatusCode::UNAUTHORIZED)
    }
}

#[utoipa::path(
    put,
    path = "/auth/login",
    tag = "Users",
    description = "Edit login information",
    request_body(content(
        (Login, example = Login::placeholder),
        (Login = "application/x-www-form-urlencoded")
    )),
    responses(
        (status = OK, description = "Ok",
            headers(
                ("Set-Cookie" = String),
                ("HX-Refresh" = String),
            ),
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
)]
#[instrument(skip(conn, encoding_key))]
pub async fn edit_login(
    DatabaseConnection(mut conn, jar, user): DatabaseConnection,
    encoding_key: Option<EncodingKeyExtractor>,
    kafka_state: KafkaState,
    TypedHeader(accept): TypedHeader<HtmlOrJsonHeader>,
    JsonOrForm(changeset_user): JsonOrForm<InsertableDatabaseUser>,
) -> Result<(CookieJar, TypedHeader<HxRefresh>), error::Error> {
    let Some(EncodingKeyExtractor(encoding_key)) = encoding_key else {
        return Err(eyre!("This service has no encoding key configured"))
            .with_status_code(StatusCode::NOT_FOUND)?;
    };

    let user_id = user.map(|u| u.id).unwrap_or_default();

    let user: User = diesel::update(users::table)
        .filter(users::id.eq(user_id))
        .set(changeset_user)
        .returning(User::as_returning())
        .get_result(&mut conn)
        .await
        .wrap_err("Failed to update user in database")
        .with_status_code(StatusCode::UNPROCESSABLE_ENTITY)?;

    kafka_state
        .producer
        .send(
            FutureRecord::<[u8], _>::to(kafka_state.user_topic.deref())
                .payload(
                    &postcard::to_stdvec(&KafkaMessage::UserInformationChanged(user.clone()))
                        .wrap_err("Failed to serialize user message")
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
        .wrap_err("Failed to send user message to kafka")
        .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

    let encoded = jsonwebtoken::encode(
        JWT_HEADER
            .get()
            .ok_or_eyre("JWT_HEADER was not set")
            .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?,
        &UserClaims {
            exp: jsonwebtoken::get_current_timestamp() + 3600,
            user,
        },
        &encoding_key,
    )
    .wrap_err("Failed to encode JWT key")
    .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((
        re_encode_cookie_jwt(encoded, jar),
        TypedHeader(HxRefresh(true)),
    ))
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
)]
#[instrument]
pub async fn logout(mut jar: CookieJar) -> (CookieJar, TypedHeader<HxRefresh>) {
    let mut session_id_num = 0;
    while jar.get(&session_id_cookie_name!(session_id_num)).is_some() {
        jar = jar.remove(session_id_cookie(session_id_num, ""));
        session_id_num += 1;
    }
    (jar, TypedHeader(HxRefresh(true)))
}

#[utoipa::path(
    delete,
    path = "/auth/login",
    tag = "Users",
    description = "Delete account",
    responses(
        (status = OK, description = "Ok",
            headers(
                ("Set-Cookie" = String)
            ),
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
)]
#[instrument(skip(conn))]
pub async fn delete_login(
    DatabaseConnection(mut conn, mut jar, user): DatabaseConnection,
) -> Result<(CookieJar, TypedHeader<HxRefresh>), error::Error> {
    let user_id = user.map(|u| u.id).unwrap_or_default();

    diesel::delete(users::table)
        .filter(users::id.eq(user_id))
        .execute(&mut conn)
        .await
        .wrap_err("Failed to delete user from database")
        .with_status_code(StatusCode::UNAUTHORIZED)?;

    let mut session_id_num = 0;
    while jar.get(&session_id_cookie_name!(session_id_num)).is_some() {
        jar = jar.remove(session_id_cookie(session_id_num, ""));
        session_id_num += 1;
    }
    Ok((jar, TypedHeader(HxRefresh(true))))
}
