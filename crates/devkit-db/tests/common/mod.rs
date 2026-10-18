//! A two-table schema in the portable style: an integer auto-increment
//! parent and a ULID-keyed child with a foreign key, a timestamp and JSON as
//! text.
#![allow(dead_code)]

use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

pub mod author {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "author")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        #[sea_orm(unique)]
        pub handle: String,
        pub created_at: ChronoDateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod post {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "post")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub author_id: i32,
        pub title: String,
        pub tags_json: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(Init)]
    }
}

#[derive(DeriveMigrationName)]
struct Init;

#[derive(DeriveIden)]
enum Author {
    Table,
    Id,
    Handle,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Post {
    Table,
    Id,
    AuthorId,
    Title,
    TagsJson,
}

#[async_trait::async_trait]
impl MigrationTrait for Init {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(Author::Table)
                .col(pk_auto(Author::Id))
                .col(string_len_uniq(Author::Handle, 8))
                .col(timestamp_with_time_zone(Author::CreatedAt))
                .to_owned(),
        )
        .await?;
        m.create_table(
            Table::create()
                .table(Post::Table)
                .col(string(Post::Id).primary_key())
                .col(integer(Post::AuthorId))
                .col(string(Post::Title))
                .col(text(Post::TagsJson))
                .foreign_key(
                    ForeignKey::create()
                        .from(Post::Table, Post::AuthorId)
                        .to(Author::Table, Author::Id),
                )
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(Post::Table).to_owned())
            .await?;
        m.drop_table(Table::drop().table(Author::Table).to_owned())
            .await
    }
}

/// A whole-second timestamp, so it compares equal after a round trip on
/// either backend (see docs/sqlite-to-postgres.md, timestamps).
pub fn now() -> chrono::DateTime<chrono::Utc> {
    use chrono::Timelike;
    chrono::Utc::now().with_nanosecond(0).unwrap()
}
