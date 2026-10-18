//! Behaviour that differs between SQLite and Postgres, pinned per backend so
//! a change shows up here first. Application code must not rely on the
//! SQLite side of any of these (docs/sqlite-to-postgres.md).

mod common;

use common::{Migrator, author, now};
use devkit_db::test_support::{Backend, backend, test_db};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, NotSet, QueryFilter, Set, TransactionTrait,
};

fn new_author(handle: &str) -> author::ActiveModel {
    author::ActiveModel {
        id: NotSet,
        handle: Set(handle.into()),
        created_at: Set(now()),
    }
}

#[tokio::test]
async fn unique_violations_are_recognised_on_both_backends() {
    let db = test_db::<Migrator>().await;
    new_author("same").insert(&db).await.unwrap();
    let err = new_author("same").insert(&db).await.unwrap_err();
    assert!(devkit_db::is_unique_violation(&err), "{err}");
}

#[tokio::test]
async fn string_length_is_enforced_only_on_postgres() {
    let db = test_db::<Migrator>().await;
    let result = new_author("longer-than-eight").insert(&db).await;
    match backend() {
        Backend::Sqlite => assert!(result.is_ok()),
        Backend::Postgres => assert!(result.is_err()),
    }
}

#[tokio::test]
async fn like_is_case_insensitive_only_on_sqlite() {
    let db = test_db::<Migrator>().await;
    new_author("Alice").insert(&db).await.unwrap();
    let found = author::Entity::find()
        .filter(author::Column::Handle.like("alice"))
        .all(&db)
        .await
        .unwrap();
    match backend() {
        Backend::Sqlite => assert_eq!(found.len(), 1),
        Backend::Postgres => assert_eq!(found.len(), 0),
    }
}

#[tokio::test]
async fn a_failed_statement_aborts_the_transaction_only_on_postgres() {
    let db = test_db::<Migrator>().await;
    new_author("taken").insert(&db).await.unwrap();

    let txn = db.begin().await.unwrap();
    assert!(new_author("taken").insert(&txn).await.is_err());
    let next = new_author("other").insert(&txn).await;
    match backend() {
        Backend::Sqlite => assert!(next.is_ok()),
        Backend::Postgres => assert!(next.is_err()),
    }
    txn.rollback().await.unwrap();
}
