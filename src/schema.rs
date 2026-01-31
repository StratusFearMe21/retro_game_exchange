// @generated automatically by Diesel CLI.

pub mod sql_types {
    #[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "condition"))]
    pub struct Condition;
}

diesel::table! {
    use diesel::sql_types::*;
    use pgvector::sql_types::*;
    use diesel_full_text_search::*;
    use super::sql_types::Condition;

    games (id) {
        id -> Int4,
        name -> Varchar,
        publisher -> Nullable<Varchar>,
        year -> Nullable<Int2>,
        platform -> Nullable<Varchar>,
        embedding -> Vector,
        condition -> Nullable<Condition>,
        owned_by -> Int4,
        tsembedding -> Tsvector,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use pgvector::sql_types::*;
    use diesel_full_text_search::*;

    users (id) {
        id -> Int4,
        username -> Varchar,
        mailing_address_1 -> Nullable<Varchar>,
        mailing_address_2 -> Nullable<Varchar>,
        city -> Nullable<Varchar>,
        #[max_length = 2]
        state -> Nullable<Bpchar>,
        zip -> Nullable<Varchar>,
        pbkdf2_iterations -> Int4,
        salt -> Bytea,
        password -> Bytea,
    }
}

diesel::joinable!(games -> users (owned_by));

diesel::allow_tables_to_appear_in_same_query!(games, users,);
