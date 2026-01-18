use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};

use clap::Parser;
use color_eyre::{
    config::Theme,
    eyre::{self, Context, bail},
};
use diesel_async::{
    AsyncMigrationHarness,
    pooled_connection::{AsyncDieselConnectionManager, bb8},
};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use serde::Deserialize;
use tokio::{net::TcpListener, signal};
use tower::ServiceBuilder;
use tower_http::{
    catch_panic::CatchPanicLayer,
    services::ServeDir,
    trace::{DefaultMakeSpan, TraceLayer},
};
use tracing::Level;
use tracing_error::ErrorLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod api;
mod cli_level_filter;
mod error;
mod html_or_json;
mod htmx;
mod json_or_form;

pub mod schema;

use cli_level_filter::CliLevelFilter;
use utoipa::openapi::{
    Info, License,
    security::{ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme},
};
use utoipa_axum::{router::OpenApiRouter, routes};
use utoipa_swagger_ui::SwaggerUi;

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

        impl $t {
            #[allow(unused_imports)]
            pub fn render_placeholder() -> String {
                use sailfish::{TemplateOnce, TemplateSimple};

                let html = biome_html_parser::parse_html(
                    &Self::placeholder()
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

use openapi_template_utoipa;

macro_rules! openapi_template {
    ($t:ty) => {
        $crate::openapi_template_utoipa!($t);
    };
    ($t:ty,$model:ident) => {
        $crate::openapi_template_utoipa!($t);
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

pub(crate) use openapi_template;

use crate::api::auth::pool::Pool;

pub trait Placeholder {
    fn placeholder() -> Self;
}

#[inline]
const fn default_listen_addr() -> SocketAddr {
    SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 3000, 0, 0))
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
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            log_level: CliLevelFilter::default(),
            addr: default_listen_addr(),
            db_url: String::new(),
        }
    }
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

    let (router, mut api) = OpenApiRouter::new()
        .routes(routes!(api::games::get_all_games, api::games::add_game))
        .routes(routes!(
            api::games::get_game,
            api::games::update_game,
            api::games::patch_game,
            api::games::delete_game
        ))
        .routes(routes!(api::auth::signup))
        .routes(routes!(api::auth::logout))
        .routes(routes!(
            api::auth::login,
            api::auth::get_login,
            api::auth::patch_login
        ))
        .split_for_parts();
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
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new("sessionid"))),
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
        .fallback_service(
            ServeDir::new("frontend/dist")
                .precompressed_gzip()
                .precompressed_br(),
        )
        .layer(
            ServiceBuilder::new()
                .layer(CatchPanicLayer::custom(error::PanicHandler))
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(DefaultMakeSpan::new().level(Level::INFO)),
                ),
        )
        .merge(SwaggerUi::new("/swagger").url("/api/openapi.json", api))
        .with_state(Pool::new(pool));

    let listener = TcpListener::bind(config.addr)
        .await
        .wrap_err_with(|| format!("Failed to open listener on {}", config.addr))?;
    tracing::info!("Listening on {}", config.addr);
    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .wrap_err("Failed to serve make service")
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
