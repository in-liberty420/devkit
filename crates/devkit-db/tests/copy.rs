//! SQLite source → the selected backend (SQLite by default, Postgres with
//! `TEST_DB_BACKEND=postgres`).

mod common;

use common::{Migrator, author, now, post};
use devkit_db::copy::{Copy, CopyError};
use devkit_db::test_support::{SharedDb, sqlite_file_db, unmigrated_test_db};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseConnection, EntityTrait, NotSet, QueryOrder, Set,
};
use sea_orm_migration::MigratorTrait;

/// A migrated SQLite source with `authors` authors, two posts each.
async fn source(authors: i32) -> SharedDb {
    let src = sqlite_file_db().await;
    let db = &src.db;
    Migrator::up(db, None).await.unwrap();
    for i in 0..authors {
        let a = author::ActiveModel {
            id: NotSet,
            handle: Set(format!("a{i}")),
            created_at: Set(now()),
        }
        .insert(db)
        .await
        .unwrap();
        for j in 0..2 {
            post::ActiveModel {
                id: Set(devkit_db::new_id()),
                author_id: Set(a.id),
                title: Set(format!("post {j} by {i}")),
                tags_json: Set(r#"["x","y"]"#.into()),
            }
            .insert(db)
            .await
            .unwrap();
        }
    }
    src
}

fn both_tables() -> Copy {
    Copy::new()
        .table::<author::Entity>()
        .table::<post::Entity>()
}

async fn all<E: EntityTrait>(db: &DatabaseConnection) -> Vec<E::Model> {
    let mut q = E::find();
    for col in <E::PrimaryKey as sea_orm::Iterable>::iter() {
        q = q.order_by_asc(sea_orm::PrimaryKeyToColumn::into_column(col));
    }
    q.all(db).await.unwrap()
}

#[tokio::test]
async fn copies_every_row_across_several_batches() {
    let src = source(7).await;
    let dst = unmigrated_test_db().await;

    let report = both_tables()
        .batch_size(3)
        .run::<Migrator>(&src.db, &dst)
        .await
        .unwrap();

    assert_eq!(
        report.tables,
        vec![("author".into(), 7), ("post".into(), 14)]
    );
    assert_eq!(
        all::<author::Entity>(&src.db).await,
        all::<author::Entity>(&dst).await
    );
    assert_eq!(
        all::<post::Entity>(&src.db).await,
        all::<post::Entity>(&dst).await
    );
}

#[tokio::test]
async fn new_rows_after_the_copy_get_fresh_auto_increment_ids() {
    let src = source(3).await;
    let dst = unmigrated_test_db().await;
    both_tables().run::<Migrator>(&src.db, &dst).await.unwrap();

    let next = author::ActiveModel {
        id: NotSet,
        handle: Set("new".into()),
        created_at: Set(now()),
    }
    .insert(&dst)
    .await
    .unwrap();
    assert_eq!(next.id, 4);
}

#[tokio::test]
async fn refuses_a_non_empty_destination_and_writes_nothing() {
    let src = source(2).await;
    let dst = unmigrated_test_db().await;
    Migrator::up(&dst, None).await.unwrap();
    author::ActiveModel {
        id: NotSet,
        handle: Set("there".into()),
        created_at: Set(now()),
    }
    .insert(&dst)
    .await
    .unwrap();

    let err = both_tables()
        .run::<Migrator>(&src.db, &dst)
        .await
        .unwrap_err();
    assert!(
        matches!(err, CopyError::DestinationNotEmpty(ref t) if t == "author"),
        "{err}"
    );
    assert_eq!(all::<post::Entity>(&dst).await.len(), 0);
}

#[tokio::test]
async fn refuses_a_source_with_rows_in_an_unlisted_table() {
    let src = source(1).await;
    let dst = unmigrated_test_db().await;

    let err = Copy::new()
        .table::<author::Entity>()
        .run::<Migrator>(&src.db, &dst)
        .await
        .unwrap_err();
    assert!(
        matches!(err, CopyError::UnlistedTables(ref t) if t == "post"),
        "{err}"
    );
}

#[tokio::test]
async fn refuses_an_unmigrated_source() {
    let src = sqlite_file_db().await;
    let dst = unmigrated_test_db().await;

    let err = both_tables()
        .run::<Migrator>(&src.db, &dst)
        .await
        .unwrap_err();
    assert!(matches!(err, CopyError::SourceNotMigrated(_)), "{err}");
}

#[tokio::test]
async fn a_failed_copy_leaves_the_destination_empty() {
    // Children listed before parents: the first post violates its foreign key.
    let src = source(2).await;
    let dst = unmigrated_test_db().await;

    let err = Copy::new()
        .table::<post::Entity>()
        .table::<author::Entity>()
        .run::<Migrator>(&src.db, &dst)
        .await
        .unwrap_err();
    match err {
        CopyError::Db(e) => assert!(devkit_db::is_foreign_key_violation(&e), "{e}"),
        other => panic!("expected a foreign-key error, got {other}"),
    }
    assert_eq!(all::<author::Entity>(&dst).await.len(), 0);
    assert_eq!(all::<post::Entity>(&dst).await.len(), 0);
}

#[tokio::test]
async fn refuses_a_table_listed_twice() {
    let src = source(1).await;
    let dst = unmigrated_test_db().await;
    let err = both_tables()
        .table::<author::Entity>()
        .run::<Migrator>(&src.db, &dst)
        .await
        .unwrap_err();
    assert!(
        matches!(err, CopyError::Duplicate(ref t) if t == "author"),
        "{err}"
    );
}

#[tokio::test]
async fn copies_into_an_already_migrated_empty_destination() {
    let src = source(2).await;
    let dst = unmigrated_test_db().await;
    Migrator::up(&dst, None).await.unwrap();

    let report = both_tables().run::<Migrator>(&src.db, &dst).await.unwrap();
    assert_eq!(
        report.tables,
        vec![("author".into(), 2), ("post".into(), 4)]
    );
}

#[tokio::test]
async fn refuses_a_destination_behind_on_migrations_before_migrating_it() {
    let src = source(1).await;
    let dst = unmigrated_test_db().await;
    Migrator::install(&dst).await.unwrap();

    let err = both_tables()
        .run::<Migrator>(&src.db, &dst)
        .await
        .unwrap_err();
    assert!(matches!(err, CopyError::DestinationNotFresh(_)), "{err}");
    assert_eq!(
        Migrator::get_pending_migrations(&dst).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn refuses_a_destination_with_tables_but_no_migration_history() {
    let src = source(1).await;
    let dst = unmigrated_test_db().await;
    Migrator::up(&dst, None).await.unwrap();
    dst.execute_unprepared("DROP TABLE seaql_migrations")
        .await
        .unwrap();

    let err = both_tables()
        .run::<Migrator>(&src.db, &dst)
        .await
        .unwrap_err();
    assert!(matches!(err, CopyError::DestinationNotFresh(_)), "{err}");
}

#[tokio::test]
async fn a_user_table_that_looks_like_sqlites_own_is_still_checked() {
    let src = source(1).await;
    src.db
        .execute_unprepared("CREATE TABLE sqlites (x INTEGER)")
        .await
        .unwrap();
    src.db
        .execute_unprepared("INSERT INTO sqlites VALUES (1)")
        .await
        .unwrap();
    let dst = unmigrated_test_db().await;

    let err = both_tables()
        .run::<Migrator>(&src.db, &dst)
        .await
        .unwrap_err();
    assert!(
        matches!(err, CopyError::UnlistedTables(ref t) if t == "sqlites"),
        "{err}"
    );
}
