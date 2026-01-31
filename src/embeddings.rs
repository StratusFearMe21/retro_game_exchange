use std::{borrow::Cow, convert::Infallible, ops::Deref, sync::Arc};

use axum::extract::FromRequestParts;
use base64::{Engine, alphabet::STANDARD, engine::GeneralPurposeConfig};
use color_eyre::eyre::{self, Context};
use diesel::{ExpressionMethods, HasQuery};
use diesel_async::{AsyncPgConnection, RunQueryDsl, pooled_connection::bb8};
use nalgebra::{ArrayStorage, Const, Matrix, U1};
use serde::{Deserialize, Deserializer, Serialize, de::Error};
use tracing::instrument;

use crate::{ApiState, api::games::InsertableGame};

const MODEL_DIMENSION: usize = 768;
const STORAGE_DIMENSION: usize = 256;

pub type VectorModelSize<T> = Matrix<T, UModelSize, U1, ArrayStorage<T, MODEL_DIMENSION, 1>>;
pub type VectorStorageSize<T> = Matrix<T, UStorageSize, U1, ArrayStorage<T, STORAGE_DIMENSION, 1>>;

pub type UModelSize = Const<MODEL_DIMENSION>;
pub type UStorageSize = Const<STORAGE_DIMENSION>;

pub async fn reembed(
    pool: bb8::Pool<AsyncPgConnection>,
    embedding_retreiver: EmbeddingRetreiver,
) -> eyre::Result<()> {
    let games = InsertableGame::query()
        .load(
            &mut pool
                .get()
                .await
                .wrap_err("Failed to get connection to DB to get games")?,
        )
        .await
        .wrap_err("Failed to get all games")?;

    let games_len = games.len();

    futures_util::future::try_join_all(games.into_iter().map(|mut game| {
        let pool = pool.clone();
        let embedding_retreiver = embedding_retreiver.clone();
        async move {
            use crate::schema::games;

            let mut conn = pool
                .get()
                .await
                .wrap_err("Failed to get connection to DB for reembedding")?;

            game.embedding = embedding_retreiver
                .get_embeddings(&format!("title: {} | text: {}", game.name, game))
                .await
                .wrap_err("Failed to create embeddings for new games")?;

            diesel::update(games::table)
                .filter(games::name.eq(&game.name))
                .set(&game)
                .execute(&mut conn)
                .await
                .wrap_err("Failed to update game in database")?;

            Ok::<_, eyre::Report>(())
        }
    }))
    .await?;

    tracing::info!("Reembedded {} games", games_len);

    Ok(())
}

#[derive(Serialize)]
pub struct EmbeddingInput<'a> {
    model: Cow<'a, str>,
    input: Cow<'a, str>,
    encoding_format: &'static str,
}

#[derive(Deserialize)]
pub struct EmbeddingResult {
    data: [EmbeddingData; 1],
}

#[derive(Deserialize)]
pub struct EmbeddingData {
    #[serde(deserialize_with = "vector_model_size_from_base64")]
    embedding: VectorModelSize<f32>,
}

fn vector_model_size_from_base64<'de, D>(deserializer: D) -> Result<VectorModelSize<f32>, D::Error>
where
    D: Deserializer<'de>,
{
    let embeddings: Cow<'de, str> = Deserialize::deserialize(deserializer)?;
    let mut embeddings_array: [u8; MODEL_DIMENSION * 4] = [0; MODEL_DIMENSION * 4];

    base64::engine::GeneralPurpose::new(&STANDARD, GeneralPurposeConfig::default())
        .decode_slice(embeddings.as_ref(), &mut embeddings_array)
        .map_err(|e| D::Error::custom(e))?;

    let embeddings_array_storage: ArrayStorage<f32, MODEL_DIMENSION, 1> =
        ArrayStorage([bytemuck::must_cast(embeddings_array)]);

    Ok(VectorModelSize::from_array_storage(
        embeddings_array_storage,
    ))
}

impl EmbeddingData {
    pub fn truncate(self) -> Vec<f32> {
        let short_embedding = self.embedding.fixed_rows::<STORAGE_DIMENSION>(0);
        short_embedding.normalize().as_slice().to_vec()
    }
}

#[derive(Clone, Debug)]
pub struct EmbeddingRetreiver {
    pub reqwest_client: reqwest::Client,
    pub embedding_model_url: Arc<str>,
    pub embedding_model_model: Arc<str>,
}

impl FromRequestParts<ApiState> for EmbeddingRetreiver {
    type Rejection = Infallible;

    async fn from_request_parts(
        _parts: &mut axum::http::request::Parts,
        state: &ApiState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self {
            reqwest_client: state.reqwest_client.clone(),
            embedding_model_url: Arc::clone(&state.embedding_model_url),
            embedding_model_model: Arc::clone(&state.embedding_model_model),
        })
    }
}

impl EmbeddingRetreiver {
    #[instrument]
    pub async fn get_embeddings(&self, input: &str) -> eyre::Result<pgvector::Vector> {
        let EmbeddingResult {
            data: [embedding_data],
        } = self
            .reqwest_client
            .post(format!("{}/embeddings", self.embedding_model_url))
            .json(&EmbeddingInput {
                model: Cow::Borrowed(self.embedding_model_model.deref()),
                input: Cow::Borrowed(input),
                encoding_format: "base64",
            })
            .send()
            .await
            .wrap_err("Failed to request embeddings from embedding URL")?
            .json()
            .await
            .wrap_err("Failed to deserialize embedding result")?;

        Ok(embedding_data.truncate().into())
    }
}
