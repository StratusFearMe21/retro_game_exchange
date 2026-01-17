// @generated automatically by Diesel CLI.

pub mod sql_types {
    #[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "condition"))]
    pub struct Condition;
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::Condition;

    games (id) {
        id -> Int4,
        name -> Varchar,
        publisher -> Nullable<Varchar>,
        year -> Nullable<Int2>,
        platform -> Nullable<Varchar>,
        condition -> Nullable<Condition>,
        owned_by -> Int4,
    }
}

diesel::table! {
    users (id) {
        id -> Int4,
        username -> Varchar,
        street_address -> Nullable<Varchar>,
        password -> Bytea,
    }
}

diesel::joinable!(games -> users (owned_by));

diesel::allow_tables_to_appear_in_same_query!(games, users,);
