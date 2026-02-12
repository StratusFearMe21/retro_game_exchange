use std::num::NonZeroU32;

use aws_lc_rs::pbkdf2;
use axum::{
    body::Bytes,
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
use color_eyre::eyre::{Context, OptionExt, eyre};
use diesel::{ExpressionMethods, HasQuery, OptionalExtension, QueryDsl};
use diesel_async::{
    AsyncPgConnection, RunQueryDsl,
    pooled_connection::bb8::{self, RunError},
};
use jsonwebtoken::{TokenData, Validation};
use tracing::instrument;

use crate::{
    ApiState,
    api::{
        auth::DatabaseUser,
        users::{User, UserClaims},
    },
    error::{self, Actions, WithStatusCode},
    schema::users,
};

#[derive(Clone)]
pub struct Pool(bb8::Pool<AsyncPgConnection>);

impl Pool {
    pub fn new(pool: bb8::Pool<AsyncPgConnection>) -> Self {
        Self(pool)
    }

    // fn get(
    //     &self,
    // ) -> impl Future<Output = Result<bb8::PooledConnection<'_, AsyncPgConnection>, RunError>> {
    //     self.0.get()
    // }

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

impl FromRequestParts<ApiState> for DatabaseConnection {
    type Rejection = error::Error;

    #[instrument(skip_all)]
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &ApiState,
    ) -> Result<Self, Self::Rejection> {
        let cookie_jar = CookieJar::from_request_parts(parts, &state.pool)
            .await
            .wrap_err("Failed to retreive cookies from header")
            .with_status_code_and_actions(StatusCode::INTERNAL_SERVER_ERROR, Actions::sign_out())?;

        let mut conn = state
            .pool
            .get_owned()
            .await
            .wrap_err("Failed to get connection to database")
            .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

        let mut user = None;

        if let Some(TypedHeader(Authorization(basic_auth))) =
            <TypedHeader<Authorization<Basic>> as OptionalFromRequestParts<ApiState>>::from_request_parts(
                parts, state,
            )
            .await
            .wrap_err("Failed to parse basic auth header")
            .with_status_code_and_actions(StatusCode::BAD_REQUEST, Actions::sign_out())?
        {
            let db_user = DatabaseUser::query()
                .filter(users::username.eq(basic_auth.username()))
                .first(&mut conn)
                .await
                .optional()
                .wrap_err("Failed to get user from database")
                .with_status_code_and_actions(StatusCode::INSUFFICIENT_STORAGE, Actions::sign_out())?
                .ok_or_eyre("Couldn't find that user")
                .with_status_code_and_actions(StatusCode::UNAUTHORIZED, Actions::sign_out())?;

            if pbkdf2::verify(super::PBKDF2_ALG, NonZeroU32::new(db_user.pbkdf2_iterations as u32).ok_or_eyre("User has invalid PBKDF2 iterations value").with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?, &db_user.salt, basic_auth.password().as_bytes(), &db_user.password).is_err() {
                return Err(eyre!("Passwords didn't match")).with_status_code_and_actions(StatusCode::UNAUTHORIZED, Actions::sign_out())
            }

            user = Some(
                User {
                    id: db_user.id,
                    username: db_user.username
                }
            );
        }

        if let Some(TypedHeader(Authorization(bearer_auth))) = <TypedHeader<
            Authorization<Bearer>,
        > as OptionalFromRequestParts<ApiState>>::from_request_parts(
            parts, state
        )
        .await
        .wrap_err("Failed to parse bearer auth header")
        .with_status_code_and_actions(StatusCode::BAD_REQUEST, Actions::sign_out())?
        .or_else(|| {
            let mut cookie_value: String = String::from("Bearer ");

            let mut session_id_num  = 0;
            while let Some(sessionid) = cookie_jar.get(&format!("sessionid.{}", session_id_num)) {
                cookie_value.push_str(sessionid.value());
                session_id_num += 1;
            }

            if cookie_value == "Bearer " {
                None
            } else {
                let cookie_bytes: Bytes = cookie_value.into();
                Some(TypedHeader(Authorization(Bearer::decode(
                    &HeaderValue::from_maybe_shared(cookie_bytes).ok()?
                )?)))
            }
        })
        {
            let header = jsonwebtoken::decode_header(bearer_auth.token())
                .wrap_err("Failed to decode JWT header")
                .with_status_code_and_actions(StatusCode::BAD_REQUEST, Actions::sign_out())?;

            let decoding_key = state.decoding_keys
                .get(
                    &header.kid
                        .ok_or_eyre("There's no kid on this JWT")
                        .with_status_code_and_actions(StatusCode::UNPROCESSABLE_ENTITY, Actions::sign_out())?
                )
                .ok_or_eyre("Couldn't find the JWK for that kid")
                .with_status_code_and_actions(StatusCode::UNPROCESSABLE_ENTITY, Actions::sign_out())?;

            let token: TokenData<UserClaims> = jsonwebtoken::decode(
                bearer_auth.token(),
                decoding_key,
                &Validation::new(jsonwebtoken::Algorithm::ES256),
            )
            .wrap_err("Failed to decode user JWT")
            .with_status_code_and_actions(StatusCode::UNAUTHORIZED, Actions::sign_out())?;

            user = Some(token.claims.user);
        }

        diesel::sql_query(r#"SELECT set_config('app.current_user_id', $1::text, false)"#)
            .bind::<diesel::sql_types::Integer, _>(user.as_ref().map(|u| u.id).unwrap_or_default())
            .execute(&mut conn)
            .await
            .wrap_err("Failed to set user id on connection")
            .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)?;

        return Ok(DatabaseConnection(conn, cookie_jar, user));
    }
}
