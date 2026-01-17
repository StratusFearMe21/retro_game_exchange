use std::{borrow::Cow, str::FromStr};

use serde::{Deserialize, Deserializer, de::Error};
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

#[derive(Clone)]
pub struct CliLevelFilter(pub EnvFilter);

impl Default for CliLevelFilter {
    fn default() -> Self {
        Self(
            EnvFilter::builder()
                .with_default_directive(DEFAULT_FILTER.into())
                .parse("")
                .unwrap(),
        )
    }
}

const DEFAULT_FILTER: LevelFilter = LevelFilter::INFO;

impl FromStr for CliLevelFilter {
    type Err = clap::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(CliLevelFilter(
            EnvFilter::builder()
                .with_default_directive(DEFAULT_FILTER.into())
                .parse(s)
                .map_err(|_| clap::Error::new(clap::error::ErrorKind::InvalidValue))?,
        ))
    }
}

impl<'de> Deserialize<'de> for CliLevelFilter {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let level: Option<Cow<'de, str>> = Deserialize::deserialize(deserializer)?;
        if let Some(level) = level {
            Ok(CliLevelFilter(
                EnvFilter::builder()
                    .with_default_directive(DEFAULT_FILTER.into())
                    .parse(level)
                    .map_err(|e| D::Error::custom(e))?,
            ))
        } else {
            Ok(CliLevelFilter(
                EnvFilter::builder()
                    .with_default_directive(DEFAULT_FILTER.into())
                    .try_from_env()
                    .or_else(|_| EnvFilter::try_new("info"))
                    .map_err(|e| D::Error::custom(e))?,
            ))
        }
    }
}
