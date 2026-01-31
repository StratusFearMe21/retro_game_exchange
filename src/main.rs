use std::{
    collections::HashMap,
    net::{Ipv6Addr, SocketAddr, SocketAddrV6, ToSocketAddrs},
    ops::Deref,
    sync::Arc,
    task::{Poll, ready},
};

use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};
use axum::{
    Json,
    body::Bytes,
    http::{
        HeaderValue, Response, StatusCode, Uri,
        header::{ACCEPT_ENCODING, Entry, LOCATION, VARY},
        uri,
    },
    routing::get,
};
use axum_extra::headers::{Header, HeaderMapExt, Vary};
use clap::{Parser, ValueEnum};
use color_eyre::{
    config::Theme,
    eyre::{self, Context, OptionExt, bail},
};
use diesel_async::{
    AsyncMigrationHarness,
    pooled_connection::{AsyncDieselConnectionManager, bb8},
};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use gethostname::gethostname;
use jsonwebtoken::{
    DecodingKey, EncodingKey,
    jwk::{Jwk, JwkSet},
};
use pin_project_lite::pin_project;
use serde::Deserialize;
use tokio::{net::TcpListener, signal};
use tower::{Layer, Service, ServiceBuilder};
use tower_http::{
    catch_panic::CatchPanicLayer,
    compression::CompressionLayer,
    services::ServeDir,
    set_header::SetResponseHeaderLayer,
    trace::{DefaultMakeSpan, TraceLayer},
};
use tracing::Level;
use tracing_error::ErrorLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use utoipa::{
    Path,
    openapi::{
        Info, License,
        security::{ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme},
    },
};
use utoipa_axum::{router::OpenApiRouter, routes};
use utoipa_swagger_ui::SwaggerUi;

mod api;
mod cli_level_filter;
mod error;
mod html_or_json;
mod htmx;
mod json_or_form;
mod uri_util;

pub mod embeddings;
pub mod schema;

use cli_level_filter::CliLevelFilter;

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!();

macro_rules! openapi_template_utoipa {
    ($t:ty) => {
        impl utoipa::PartialSchema for $t {
            fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::Schema> {
                utoipa::openapi::RefOr::T(utoipa::openapi::Schema::Object(
                    utoipa::openapi::ObjectBuilder::new()
                        .schema_type(utoipa::openapi::schema::SchemaType::new(
                            utoipa::openapi::Type::String,
                        ))
                        .build(),
                ))
            }
        }

        impl utoipa::ToSchema for $t {
            fn name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(stringify!($t))
            }

            fn schemas(
                _schemas: &mut Vec<(String, utoipa::openapi::RefOr<utoipa::openapi::Schema>)>,
            ) {
            }
        }
    };
}

pub(crate) use openapi_template_utoipa;

macro_rules! openapi_template_render {
    ($t:ty,$fn_name:ident,$fn:ident) => {
        impl $t {
            #[allow(unused_imports)]
            pub fn $fn_name() -> String {
                use sailfish::{TemplateOnce, TemplateSimple};

                let html = biome_html_parser::parse_html(
                    &Self::$fn()
                        .render_once()
                        .unwrap_or_else(|_| "Failed to render example".to_owned()),
                    biome_html_parser::HtmlParseOptions::default(),
                );

                let Ok(formatted) = biome_html_formatter::format_node(
                    biome_html_formatter::context::HtmlFormatOptions::default()
                        .with_indent_style(biome_formatter::IndentStyle::Space),
                    &html.syntax(),
                    false,
                ) else {
                    return "Failed to format example".to_owned();
                };
                let Ok(printed) = formatted.print() else {
                    return "Failed to print example".to_owned();
                };
                printed.into_code()
            }
        }
    };
}

pub(crate) use openapi_template_render;

macro_rules! openapi_template_serialize {
    ($t:ty,$model:ident) => {
        impl serde::Serialize for $t {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serde::Serialize::serialize(&self.$model, serializer)
            }
        }
    };
}

pub(crate) use openapi_template_serialize;

use crate::{
    api::auth::{JWT_HEADER, pool::Pool},
    embeddings::EmbeddingRetreiver,
    htmx::{HxLocation, HxRequest},
};

pub trait Placeholder {
    fn placeholder() -> Self;
}

#[inline]
const fn default_listen_addr() -> SocketAddr {
    SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 3000, 0, 0))
}

#[derive(ValueEnum, Deserialize, Clone, Copy, Default)]
#[serde(rename_all = "lowercase")]
enum ServiceType {
    Auth,
    #[default]
    Main,
}

#[derive(Parser, Deserialize)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[clap(short, long, env = "RUST_LOG")]
    #[serde(default)]
    log_level: CliLevelFilter,
    #[clap(short, long, env = "LISTEN_ADDR")]
    #[serde(default = "default_listen_addr")]
    addr: SocketAddr,
    #[clap(short, long, env = "DATABASE_URL")]
    #[serde(default)]
    db_url: String,
    #[clap(long, env = "EMBEDDING_MODEL_URL")]
    #[serde(default)]
    embedding_model_url: Arc<str>,
    #[clap(long, env = "EMBEDDING_MODEL_MODEL")]
    #[serde(default)]
    embedding_model_model: Arc<str>,
    #[clap(long)]
    #[serde(skip)]
    reembed: bool,
    #[clap(short, long, value_enum, env = "SERVICE_TYPE")]
    #[serde(default)]
    service_type: ServiceType,
    #[clap(long, env = "AUTH_SERVICE")]
    #[serde(default)]
    auth_service: String,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            log_level: CliLevelFilter::default(),
            addr: default_listen_addr(),
            db_url: String::new(),
            embedding_model_url: String::new().into(),
            embedding_model_model: String::new().into(),
            reembed: false,
            service_type: ServiceType::default(),
            auth_service: String::new(),
        }
    }
}

#[derive(Clone)]
pub struct ApiState {
    pub pool: Pool,
    pub embedding_model_url: Arc<str>,
    pub embedding_model_model: Arc<str>,
    pub reqwest_client: reqwest::Client,
    pub encoding_key: Option<EncodingKey>,
    pub decoding_keys: Arc<HashMap<String, DecodingKey>>,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    dotenvy::dotenv().ok();
    let color = supports_color::on(supports_color::Stream::Stderr)
        .map(|c| c.has_basic)
        .unwrap_or_default();

    color_eyre::config::HookBuilder::default()
        .theme(if color {
            Theme::dark()
        } else {
            Theme::default()
        })
        .display_env_section(false)
        .install()?;

    let mut config = match std::fs::read_to_string("config.toml") {
        Ok(file) => toml::from_str(&file).wrap_err("Failed to deserialize config file")?,
        Err(e) => {
            eprintln!("Failed to open config file: {}", e);
            eprintln!("Using default config");
            Cli::default()
        }
    };
    config.update_from(std::env::args_os());

    tracing_subscriber::registry()
        .with(ErrorLayer::default())
        .with(config.log_level.0)
        .with(tracing_subscriber::fmt::layer().with_ansi(color))
        .init();

    if config.db_url.is_empty() {
        bail!("db_url is not set");
    }

    let db_config =
        AsyncDieselConnectionManager::<diesel_async::AsyncPgConnection>::new(config.db_url);
    let pool = bb8::Pool::builder()
        .build(db_config)
        .await
        .wrap_err("Failed to build database pool")?;

    let mut harness = AsyncMigrationHarness::new(
        pool.get_owned()
            .await
            .wrap_err("Failed to get owned connection to database")?,
    );
    // SAFETY: Box<dyn Error + Send + Sync> is not also 'static,
    // so must use unwrap
    harness.run_pending_migrations(MIGRATIONS).unwrap();

    if config.reembed {
        embeddings::reembed(
            pool,
            EmbeddingRetreiver {
                reqwest_client: reqwest::Client::new(),
                embedding_model_url: Arc::clone(&config.embedding_model_url),
                embedding_model_model: Arc::clone(&config.embedding_model_model),
            },
        )
        .await?;
        return Ok(());
    }

    let reqwest_client = reqwest::Client::new();

    let (encoding_key, jwk_set) = match config.service_type {
        ServiceType::Auth => {
            let jwt_key_pair = EcdsaKeyPair::generate(&ECDSA_P256_SHA256_FIXED_SIGNING)
                .wrap_err("Failed to generate ECDSA keypair")?;

            let encoding_key = EncodingKey::from_ec_der(
                jwt_key_pair
                    .to_pkcs8v1()
                    .wrap_err("Failed to serialize encoding key to DER")?
                    .as_ref(),
            );

            let mut jwk = Jwk::from_encoding_key(&encoding_key, jsonwebtoken::Algorithm::ES256)
                .wrap_err("Failed to create JWK from JWT EncodingKey")?;

            let key_id = Some(gethostname().into_string().unwrap());
            jwk.common.key_id = key_id.clone();

            JWT_HEADER
                .set({
                    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
                    header.jwk = Some(jwk.clone());
                    header.kid = key_id;
                    header
                })
                .unwrap();

            let jwk_set = JwkSet { keys: vec![jwk] };

            (Some(encoding_key), jwk_set)
        }
        ServiceType::Main => {
            let jwks_url =
                reqwest::Url::parse(&format!("{}/.well-known/jwks.json", config.auth_service))
                    .wrap_err("Failed to parse auth_service as URL")?;

            let auth_service_host = jwks_url
                .host_str()
                .ok_or_eyre("No hostname was found in the auth_service URL")?;

            let jwk_set: JwkSet = JwkSet {
                keys: futures_util::future::try_join_all(
                    (auth_service_host, jwks_url.port().unwrap_or_default())
                        .to_socket_addrs()
                        .wrap_err("Failed to resolve address for auth service")?
                        .map(|addr| {
                            let jwks_url = jwks_url.clone();
                            async move {
                                let mut jwks: JwkSet = reqwest::ClientBuilder::new()
                                    .resolve(auth_service_host, addr)
                                    .build()
                                    .wrap_err("Failed to build reqwest client for JWKs requst")?
                                    .get(jwks_url)
                                    .send()
                                    .await
                                    .wrap_err("Failed to request JWKs from auth service")?
                                    .json()
                                    .await
                                    .wrap_err("Failed to deserialize JWKs from auth service")?;

                                Ok::<_, eyre::Report>(jwks.keys.remove(0))
                            }
                        }),
                )
                .await?,
            };

            (None, jwk_set)
        }
    };

    let decoding_keys = Arc::new(
        jwk_set
            .keys
            .iter()
            .map(|k| {
                (
                    k.common.key_id.clone().unwrap(),
                    DecodingKey::from_jwk(k)
                        .wrap_err("Failed to derive decoding key from auth service JWKs")
                        .unwrap(),
                )
            })
            .collect::<HashMap<_, _>>(),
    );

    let router = OpenApiRouter::new().routes(routes!(api::health));

    let (main_router, main_api) = router
        .clone()
        .routes(routes!(api::games::get_all_games, api::games::add_game))
        .routes(routes!(
            api::games::get_game,
            api::games::update_game,
            api::games::patch_game,
            api::games::delete_game
        ))
        .routes(routes!(api::users::get_user))
        .split_for_parts();

    let (auth_router, auth_api) = router
        .routes(routes!(api::auth::signup))
        .routes(routes!(api::auth::logout))
        .routes(routes!(
            api::auth::login,
            api::auth::edit_login,
            api::auth::delete_login
        ))
        .split_for_parts();

    let (router, mut api) = match config.service_type {
        ServiceType::Main => (main_router, main_api.merge_from(auth_api)),
        ServiceType::Auth => (auth_router, auth_api.merge_from(main_api)),
    };

    api.info = Info::builder()
        .title(env!("CARGO_PKG_NAME"))
        .description(option_env!("CARGO_PKG_DESCRIPTION"))
        .version(env!("CARGO_PKG_VERSION"))
        .license(
            option_env!("CARGO_PKG_LICENSE")
                .map(|license| License::builder().identifier(Some(license)).build()),
        )
        .contact(None)
        .build();
    api.components.as_mut().map(|components| {
        components.security_schemes.insert(
            "cookie_jwt".to_owned(),
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new("sessionid.0"))),
        );
        components.security_schemes.insert(
            "bearer_jwt".to_owned(),
            SecurityScheme::Http(
                Http::builder()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .build(),
            ),
        );
        components.security_schemes.insert(
            "basic_auth".to_owned(),
            SecurityScheme::Http(
                Http::builder()
                    .scheme(HttpAuthScheme::Basic)
                    // .bearer_format("JWT")
                    .build(),
            ),
        );
    });
    let app = router
        .route(
            "/.well-known/jwks.json",
            get(move || async move { Json(jwk_set) }),
        )
        .merge(SwaggerUi::new("/swagger").url("/api/openapi.json", api))
        .fallback_service(
            ServiceBuilder::new()
                .layer(HxRequestLayer)
                .layer(SetResponseHeaderLayer::appending(
                    VARY,
                    HeaderValue::from(ACCEPT_ENCODING),
                ))
                .service(
                    ServeDir::new("frontend/dist")
                        .precompressed_gzip()
                        .precompressed_br(),
                ),
        )
        .layer(
            ServiceBuilder::new()
                .layer(CatchPanicLayer::custom(error::PanicHandler))
                .layer(RedirectIfUnauthorizedLayer::to(
                    "/login.html",
                    [
                        api::auth::__path_login::path(),
                        api::auth::__path_signup::path(),
                    ],
                ))
                .layer(CompressionLayer::new().br(true).gzip(true))
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(DefaultMakeSpan::new().level(Level::INFO)),
                ),
        )
        .with_state(ApiState {
            pool: Pool::new(pool),
            embedding_model_url: Arc::clone(&config.embedding_model_url),
            embedding_model_model: Arc::clone(&config.embedding_model_model),
            reqwest_client,
            encoding_key,
            decoding_keys,
        });

    let listener = TcpListener::bind(config.addr)
        .await
        .wrap_err_with(|| format!("Failed to open listener on {}", config.addr))?;
    tracing::info!("Listening on {}", config.addr);
    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .wrap_err("Failed to serve make service")
}

pub struct HxRequestLayer;

impl<S> Layer<S> for HxRequestLayer {
    type Service = ServeHtmxDir<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ServeHtmxDir(inner)
    }
}

#[derive(Clone)]
pub struct RedirectIfUnauthorizedLayer {
    path: Arc<str>,
    exceptions: Arc<[String]>,
}

impl RedirectIfUnauthorizedLayer {
    fn to(path: impl Into<Arc<str>>, exceptions: impl Into<Arc<[String]>>) -> Self {
        Self {
            path: path.into(),
            exceptions: exceptions.into(),
        }
    }
}

impl<S> Layer<S> for RedirectIfUnauthorizedLayer {
    type Service = RedirectIfUnauthorized<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RedirectIfUnauthorized {
            path: Arc::clone(&self.path),
            exceptions: Arc::clone(&self.exceptions),
            inner,
        }
    }
}

#[derive(Clone)]
pub struct ServeHtmxDir<S>(S);

impl<S, ReqBody, ResBody> Service<axum::http::Request<ReqBody>> for ServeHtmxDir<S>
where
    S: Service<axum::http::Request<ReqBody>, Response = Response<ResBody>>,
{
    type Response = Response<ResBody>;
    type Error = S::Error;
    type Future = ServeHtmxDirFuture<S::Future>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, mut req: axum::http::Request<ReqBody>) -> Self::Future {
        let mut values = req.headers().get_all(HxRequest::name()).iter();
        let hx_request = HxRequest::decode(&mut values).unwrap_or_default();
        let mut vary_header = false;

        if req.uri().path().ends_with(".html") {
            vary_header = true;

            if hx_request.0 {
                let uri = std::mem::take(req.uri_mut());
                let mut parts = uri.into_parts();

                let path_and_query = parts.path_and_query.unwrap();
                let path = path_and_query.path();
                let joined_path = uri_util::join_path("/partials/", path.trim_start_matches("/"));
                let joined_path_bytes: Bytes = joined_path.into();
                let joined_path_and_query =
                    uri::PathAndQuery::from_maybe_shared(joined_path_bytes).unwrap();
                parts.path_and_query = Some(joined_path_and_query);

                if let Ok(uri) = Uri::from_parts(parts) {
                    *req.uri_mut() = uri;
                };
            }
        }

        ServeHtmxDirFuture {
            future: self.0.call(req),
            vary_header,
        }
    }
}

#[derive(Clone)]
pub struct RedirectIfUnauthorized<S> {
    path: Arc<str>,
    exceptions: Arc<[String]>,
    inner: S,
}

impl<S, ReqBody, ResBody> Service<axum::http::Request<ReqBody>> for RedirectIfUnauthorized<S>
where
    S: Service<axum::http::Request<ReqBody>, Response = Response<ResBody>>,
{
    type Response = Response<ResBody>;
    type Error = S::Error;
    type Future = RedirectIfUnauthorizedFuture<S::Future>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: axum::http::Request<ReqBody>) -> Self::Future {
        let mut hx_request_values = req.headers().get_all(HxRequest::name()).iter();
        let hx_request = HxRequest::decode(&mut hx_request_values).unwrap_or_default();
        let path = req.uri().path();
        RedirectIfUnauthorizedFuture {
            exceptions_matched: self.exceptions.iter().any(|e| e.as_str() == path),
            path: Arc::clone(&self.path),
            future: self.inner.call(req),
            hx_request,
        }
    }
}

pin_project! {
    pub struct ServeHtmxDirFuture<F> {
        #[pin]
        future: F,
        vary_header: bool,
    }
}

impl<F, ResBody, E> Future for ServeHtmxDirFuture<F>
where
    F: Future<Output = Result<Response<ResBody>, E>>,
{
    type Output = F::Output;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();
        let mut res = ready!(this.future.poll(cx)?);

        if *this.vary_header {
            let res_header = res.headers_mut().entry(Vary::name());
            let vary_header = Vary::from(HxRequest::name().clone());
            vary_header.encode(&mut AppendToHeader(Some(res_header)));
        }

        Poll::Ready(Ok(res))
    }
}

pin_project! {
    pub struct RedirectIfUnauthorizedFuture<F> {
        #[pin]
        future: F,
        path: Arc<str>,
        exceptions_matched: bool,
        hx_request: HxRequest
    }
}

impl<F, ResBody, E> Future for RedirectIfUnauthorizedFuture<F>
where
    F: Future<Output = Result<Response<ResBody>, E>>,
{
    type Output = F::Output;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();
        let mut res = ready!(this.future.poll(cx)?);

        if !*this.exceptions_matched && res.status() == StatusCode::UNAUTHORIZED {
            if this.hx_request.0 {
                res.headers_mut()
                    .typed_insert(HxLocation::new(Arc::clone(this.path).deref().to_owned()));
            } else {
                *res.status_mut() = StatusCode::FOUND;

                res.headers_mut()
                    .insert(LOCATION, HeaderValue::from_str(this.path.deref()).unwrap());
            }
        }

        Poll::Ready(Ok(res))
    }
}

pub struct AppendToHeader<'a>(Option<Entry<'a, HeaderValue>>);

impl<'a> Extend<HeaderValue> for AppendToHeader<'a> {
    fn extend<T: IntoIterator<Item = HeaderValue>>(&mut self, iter: T) {
        for value in iter {
            self.0 = match self.0.take() {
                Some(Entry::Occupied(mut e)) => {
                    e.append(value);
                    Some(Entry::Occupied(e))
                }
                Some(Entry::Vacant(e)) => Some(Entry::Occupied(e.insert_entry(value))),
                None => None,
            }
        }
    }
}

#[derive(Default, Deserialize, Debug)]
pub struct SearchQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub uid: i32,
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
